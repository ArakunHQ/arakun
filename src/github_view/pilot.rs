//! "Tu agente": the cards arakun-pilot leaves (`crate::github::pilot`), at the
//! top of the queue. Their text is Spanish like the items, and every write to
//! GitHub goes through the tab's own confirm strip, as the user, with `gh`.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Focusable as _, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};

use super::{Confirm, GitHubView, NEWLINE, Note, Tab};
use crate::github::Problem;
use crate::github::pilot::{self, Item, Kind, Riesgo};
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

/// Why an approval from a card is refused: the pull request moved on.
fn moved_text(number: u64, read: &str, now: &str) -> String {
    let short = |s: &str| s.chars().take(7).collect::<String>();
    format!(
        "#{number} tiene commits nuevos desde el resumen ({} → {}). No lo aprobé: revísalo otra vez.",
        short(read),
        short(now)
    )
}

/// The approval a card sends: only for the commit the agent read, and only
/// while it is still the head.
fn approve_job(item: &ItemRef, body: &str, head: &str) -> Result<(), Problem> {
    let now = pr::head_sha(item)?;
    if !pilot::same_commit(&now, head) {
        return Err(Problem::Other(moved_text(item.number, head, &now)));
    }
    pr::review_at(item, Verdict::Approve, body, head)
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

impl GitHubView {
    /// The section's heading and its cards; nothing when no card waits.
    pub(super) fn pilot_section(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = self.pilot.read(cx);
        let items: Vec<Item> = p.shown().cloned().collect();
        let problem = p.problem().map(str::to_string);
        if items.is_empty() && problem.is_none() {
            return Vec::new();
        }
        let mut out = vec![super::queue::group_header("Tu agente", items.len()).into_any_element()];
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
                            .child(format!("No pude leer a tu agente: {e}")),
                    )
                    .into_any_element(),
            );
        }
        for (ix, item) in items.iter().enumerate() {
            out.push(self.render_pilot_card(ix, item, cx));
        }
        out
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
                    "Importante",
                    theme::attention(),
                ))
            })
            .child(div().flex_1())
            .child(super::mono(super::ago(when), theme::text_subtle()).flex_none());
        // Like a queue row's second line: the number, then who.
        let meta = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(6.))
            .min_w_0()
            .when(item.number > 0, |d| {
                d.child(super::mono(format!("#{}", item.number), theme::text_subtle()).flex_none())
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
                        "1 punto".to_string()
                    } else {
                        format!("{n} puntos")
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
                                        .tooltip(ui::tip("Ver los archivos del PR"))
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
                "Ver",
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
                None,
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
                            "Visto bueno",
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
                            "Pedir cambios",
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
            Kind::NeedsYou | Kind::Failed | Kind::Other => {
                buttons = buttons.child(see).child(done("Listo", false, cx));
            }
        }
        let mut foot = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .child(buttons);
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

    /// "Visto bueno": the confirm strip for an approval of the commit the
    /// agent read. A push since then cancels it.
    fn pilot_approve(&mut self, item: &Item, cx: &mut Context<Self>) {
        let Some(r) = item.item_ref() else { return };
        self.pilot_notes.remove(&item.id);
        if item.head.is_empty() {
            self.pilot_notes.insert(
                item.id.clone(),
                "El resumen no dice para qué commit es; apruébalo desde el PR.".into(),
            );
            cx.notify();
            return;
        }
        self.pilot_see(item, cx);
        self.ask_confirm(Confirm::Approve, cx);
        let act = PilotAct {
            id: item.id.clone(),
            item: r.clone(),
            head: item.head.clone(),
        };
        self.pilot_act = Some(act.clone());
        // Asked now so the strip goes before anyone types; the approval asks again.
        cx.spawn(async move |this, cx| {
            let it = r.clone();
            let now = cx.background_spawn(async move { pr::head_sha(&it) }).await;
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
    }

    /// "Pedir cambios": the confirm strip with the agent's review in it, to
    /// edit or send as it is.
    fn pilot_request_changes(&mut self, item: &Item, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = item.item_ref() else { return };
        self.pilot_notes.remove(&item.id);
        self.pilot_see(item, cx);
        self.ask_confirm(Confirm::RequestChanges, cx);
        let text = item.borrador.trim_end().replace('\n', NEWLINE);
        self.body_input.update(cx, |i, cx| i.set_text(text, cx));
        window.focus(&self.body_input.focus_handle(cx), cx);
        self.pilot_act = Some(PilotAct {
            id: item.id.clone(),
            item: r,
            head: item.head.clone(),
        });
    }

    /// Ok, Listo, or a card's write that GitHub took: the card goes.
    pub(super) fn pilot_finish(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.pilot_act.as_ref().is_some_and(|a| a.id == id) {
            self.pilot_act = None;
        }
        self.pilot_notes.remove(id);
        self.pilot_open.remove(id);
        if self.pilot_focus.as_deref() == Some(id) {
            self.pilot_focus = None;
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
        let after = move |this: &mut Self, cx: &mut Context<Self>| {
            this.confirm = None;
            this.pilot_finish(&id, cx);
        };
        let head = act.head;
        match c {
            Confirm::Approve => self.write(
                "approve",
                "Visto bueno enviado.".into(),
                move |i| approve_job(i, &body, &head),
                after,
                cx,
            ),
            Confirm::RequestChanges => {
                if body.trim().is_empty() {
                    self.note = Some(Note::failed("Escribe qué debe cambiar."));
                    cx.notify();
                    return;
                }
                let pinned = !head.is_empty();
                self.write(
                    "request-changes",
                    "Cambios pedidos.".into(),
                    move |i| {
                        if pinned {
                            pr::review_at(i, Verdict::RequestChanges, &body, &head)
                        } else {
                            pr::review(i, Verdict::RequestChanges, &body, "")
                        }
                    },
                    after,
                    cx,
                )
            }
            _ => {}
        }
    }

    /// From the island's "Ver": the card in view, its pull request picked.
    pub fn reveal_pilot(&mut self, id: &str, cx: &mut Context<Self>) {
        let found = self.pilot.read(cx).find(id).cloned();
        self.queue_scroll.set_offset(gpui::Point::default());
        if let Some(item) = found {
            self.pilot_see(&item, cx);
        }
    }

    /// The confirm strip's words when it came from a card.
    pub(super) fn pilot_question(&self, c: Confirm) -> Option<(String, String)> {
        let act = self.pilot_for(c)?;
        let n = act.item.number;
        Some(match c {
            Confirm::Approve => (format!("¿Visto bueno a #{n}, como tú?"), "Aprobar".into()),
            _ => (format!("¿Pedir cambios en #{n}?"), "Pedir cambios".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moved_head_says_both_commits() {
        assert_eq!(
            moved_text(95, "40a1b2c3d4e5f6", "99ffeedd00112233"),
            "#95 tiene commits nuevos desde el resumen (40a1b2c → 99ffeed). No lo aprobé: revísalo otra vez."
        );
    }
}
