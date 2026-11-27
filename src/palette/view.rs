//! Drawing the palette: query line, tabs, the list with its cards, the
//! preview, the footer, and the actions menu and bulk bar that float over it.

use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, Context, ElementId, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};

use super::*;
use crate::input::{Backspace, Copy as CopyText, Left, Right, SelectAll};
use crate::theme;
use crate::ui::icon;
use crate::ui::{self as kit, Tint, Type, Typography as _};

fn id_of(key: &str) -> u64 {
    key.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

pub(super) fn lead(lead: &Lead) -> AnyElement {
    match lead {
        Lead::Icon(name, tint) => kit::lead_icon(name, *tint).into_any_element(),
        Lead::Avatar(login, path) => {
            kit::ringed_avatar(login, path.as_deref(), 20.).into_any_element()
        }
        Lead::Working => kit::lead_spinner(),
        Lead::Idle => kit::lead_ring().into_any_element(),
        Lead::Dot(tint, halo) => kit::lead_dot(tint.color(), *halo).into_any_element(),
    }
}

/// A small line of text in a card.
fn caption(text: impl Into<SharedString>, color: gpui::Hsla) -> gpui::Div {
    div()
        .min_w_0()
        .ty(Type::Caption)
        .text_color(color)
        .child(text.into())
}

/// The bar beside the selected row, growing in from its middle.
fn bar(key: &str) -> AnyElement {
    kit::selection_bar()
        .with_animation(
            ElementId::NamedInteger("palette-bar".into(), id_of(key)),
            Animation::new(crate::ui::motion::FAST).with_easing(crate::ui::motion::ease_out_back),
            |d, t| {
                d.top(px(9. + 10. * (1. - t)))
                    .bottom(px(9. + 10. * (1. - t)))
            },
        )
        .into_any_element()
}

impl Palette {
    fn render_query(&self, cx: &mut Context<Self>) -> AnyElement {
        let q = kit::palette_query()
            .child(icon("search01", 18.).text_color(theme::text_faint()))
            .children(
                self.pages
                    .iter()
                    .flat_map(|p| p.crumbs())
                    .flat_map(|(ic, label)| {
                        [
                            kit::crumb(ic, label).into_any_element(),
                            kit::crumb_separator().into_any_element(),
                        ]
                    }),
            )
            .when_some(self.scope, |d, s| {
                d.child(kit::palette_scope_glyph(s.glyph()))
            })
            .children(self.chips.iter().map(|c| match c {
                Chip::Model { label, .. } => kit::token_chip("bot", label.clone(), Tint::Violet),
                Chip::Branch(b) => kit::token_chip("git-branch", b.clone(), Tint::Green),
            }))
            .child(div().flex_1().min_w_0().child(self.input.clone()));
        let _ = cx;
        q.into_any_element()
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let leads = self.entries.first().is_some_and(|e| {
            matches!(
                e.card,
                Some(
                    Card::Answer
                        | Card::Rule(_)
                        | Card::Todo(_)
                        | Card::Confirm(..)
                        | Card::Cloud(_)
                )
            )
        });
        if !self.pages.is_empty() || leads || self.tabs.len() <= 1 {
            return None;
        }
        let home = self.query_text(cx).is_empty() && self.scope.is_none();
        Some(
            kit::palette_tabs()
                .children(self.tabs.iter().map(|(tab, n)| {
                    let tab = *tab;
                    kit::palette_tab(
                        ElementId::Name(format!("palette-tab-{}", tab.label()).into()),
                        tab.icon(),
                        tab.label(),
                        // The All tab counts only once something is typed.
                        if tab == Tab::All && home {
                            None
                        } else {
                            Some(*n)
                        },
                        tab == self.tab,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
                }))
                .into_any_element(),
        )
    }

    fn query_text(&self, cx: &App) -> String {
        self.input.read(cx).text().trim().to_string()
    }

    fn render_row(&self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let e = &self.entries[i];
        if let Some(card) = &e.card {
            return self.render_card(i, card, cx);
        }
        let selected = i == self.selected;
        let item = &e.item;
        let marking = !self.marked.is_empty();
        let marked = self.marked.contains(&item.key);
        let pinned = self.pins.has(&item.key);
        let end = kit::row_end(selected)
            .when_some(item.pill.clone(), |d, (text, tint)| {
                d.child(kit::palette_pill(text, tint.color()))
            })
            .when(item.toggle.is_some(), |d| {
                d.child(kit::switch(item.toggle == Some(true)))
            })
            .when(!selected && !item.keys.is_empty(), |d| {
                d.child(kit::row_meta(item.keys.join(" ")))
            })
            .when(selected && !item.keys.is_empty(), |d| {
                d.child(kit::keycaps(&item.keys))
            })
            .when(
                !item.meta.is_empty() && (!selected || item.keys.is_empty()),
                |d| d.child(kit::row_meta(item.meta.clone())),
            )
            .when(pinned && !selected, |d| {
                d.child(icon("pin", 12.).text_color(theme::text_faint()))
            })
            .when(selected && item.toggle.is_none(), |d| {
                d.child(kit::keycap("↵"))
            });
        kit::palette_row(
            ElementId::NamedInteger("palette-row".into(), i as u64),
            item.title.clone(),
            selected,
        )
        .when(!item.sub.is_empty(), |d| {
            d.aria_description(item.sub.clone())
        })
        .when_some(item.toggle, |d, on| d.aria_toggled(crate::ui::toggled(on)))
        .when(selected, |d| d.child(bar(&item.key)))
        .child(if marking {
            kit::check_box(marked).into_any_element()
        } else {
            lead(&item.lead)
        })
        .child(kit::row_title(
            item.title.clone(),
            &e.ranges,
            selected,
            item.dim,
        ))
        .when(!item.sub.is_empty(), |d| {
            d.child(kit::row_sub(item.sub.clone(), selected))
        })
        .child(end)
        .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
            this.selected = i;
            if ev.modifiers().secondary(cx) || ev.modifiers().shift {
                if let Some(k) = this.entries.get(i).map(|e| e.item.key.clone())
                    && !this.marked.remove(&k)
                {
                    this.marked.insert(k);
                }
                cx.notify();
            } else {
                this.pick(i, cx);
            }
        }))
        .into_any_element()
    }

    fn render_card(&self, i: usize, card: &Card, cx: &mut Context<Self>) -> AnyElement {
        let selected = i == self.selected;
        let enter = || kit::keycap("↵");
        let q = self.query_text(cx);
        let (tint, body): (Tint, AnyElement) = match card {
            Card::Clip(clip::Clip::GitHub { url, link }) => {
                let (title, sub) = match link {
                    clip::GitHubLink::Pull { number, repo }
                    | clip::GitHubLink::Issue { number, repo } => {
                        let known = self.seed.items.iter().find(|it| {
                            matches!(&it.target, Target::Pull(r) if r.number == *number && r.repo.eq_ignore_ascii_case(repo))
                        });
                        let what = if matches!(link, clip::GitHubLink::Pull { .. }) {
                            "PR"
                        } else {
                            "issue"
                        };
                        (
                            format!("Open {what} #{number} from your clipboard"),
                            known
                                .map(|k| k.title.to_string())
                                .unwrap_or_else(|| short_url(url)),
                        )
                    }
                    clip::GitHubLink::Run { id, .. } => {
                        (format!("Open run {id} from your clipboard"), short_url(url))
                    }
                    clip::GitHubLink::Commit { sha, .. } => (
                        format!(
                            "Open commit {} from your clipboard",
                            &sha[..7.min(sha.len())]
                        ),
                        short_url(url),
                    ),
                };
                (
                    Tint::Blue,
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(12.))
                        .w_full()
                        .child(kit::glow_tile("github", Tint::Blue))
                        .child(kit::card_text(
                            title,
                            Some(div().child(sub).into_any_element()),
                        ))
                        .when(selected, |d| d.child(enter()))
                        .into_any_element(),
                )
            }
            Card::Clip(clip::Clip::Error(e)) => (
                Tint::Red,
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .w_full()
                    .child(kit::glow_tile("alert-circle", Tint::Red))
                    .child(kit::card_text(
                        format!("From your clipboard: {}", e.language),
                        Some(
                            kit::mono(
                                match &e.location {
                                    Some((p, l)) => format!("{} · {p}:{l}", e.headline),
                                    None => e.headline.clone(),
                                },
                                theme::text_muted(),
                            )
                            .truncate()
                            .into_any_element(),
                        ),
                    ))
                    .when(selected, |d| d.child(kit::keycaps(&["↵"])))
                    .into_any_element(),
            ),
            Card::Ask => {
                let model = self
                    .model_chip()
                    .map(|(_, l)| l)
                    .unwrap_or_else(|| self.seed.agent.clone());
                let branch = self.branch_chip();
                let title = match &branch {
                    Some(b) => format!("Ask {model} in {b}: “{q}”"),
                    None => format!("Ask {model} about “{q}”"),
                };
                let sub: AnyElement = match (self.failure(), &branch) {
                    (Some(f), Some(_)) => div()
                        .flex()
                        .gap(crate::ui::sp(4.))
                        .child("With the last failed run as context ·")
                        .child(div().text_color(theme::error()).child(f.what.clone()))
                        .into_any_element(),
                    _ => div()
                        .child(format!(
                            "New session in {} · {}",
                            self.seed.project,
                            branch.clone().unwrap_or_else(|| self.seed.branch.clone())
                        ))
                        .into_any_element(),
                };
                (
                    Tint::Amber,
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(12.))
                        .w_full()
                        .child(kit::glow_tile("sparkles", Tint::Amber))
                        .child(kit::card_text(title, Some(sub)))
                        .when(selected, |d| d.child(kit::keycaps(&["Ctrl", "↵"])))
                        .into_any_element(),
                )
            }
            Card::OfferAnswer => (
                Tint::Neutral,
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .w_full()
                    .child(kit::glow_tile("sparkles", Tint::Amber))
                    .child(kit::card_text(
                        format!("Quick answer from {}", answer::MODEL_LABEL),
                        Some(
                            div()
                                .child("Enter to ask Claude. Nothing is sent before.")
                                .into_any_element(),
                        ),
                    ))
                    .when(selected, |d| d.child(enter()))
                    .into_any_element(),
            ),
            Card::OfferRule => {
                let reading = self.reading_rule.as_deref() == Some(q.as_str());
                (
                    Tint::Violet,
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(12.))
                        .w_full()
                        .child(kit::glow_tile("flash", Tint::Violet))
                        .child(kit::card_text(
                            if reading {
                                "Reading it as an automation…"
                            } else {
                                "Read as an automation"
                            },
                            Some(
                                div()
                                    .child(if reading {
                                        "Claude is reading the sentence. Typing or closing stops it."
                                    } else {
                                        "Enter to have Claude read it. Nothing is sent before."
                                    })
                                    .into_any_element(),
                            ),
                        ))
                        .when(selected && !reading, |d| d.child(enter()))
                        .into_any_element(),
                )
            }
            Card::Remember(m) => (
                Tint::Neutral,
                div()
                    .flex()
                    .items_start()
                    .gap(crate::ui::sp(12.))
                    .w_full()
                    .child(kit::glow_tile("bot", Tint::Amber))
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
                                    .child(kit::card_text("Arakun remembers", None).flex_none())
                                    .child(kit::palette_pill(
                                        format!("{} {}", m.memory.kind, m.id()),
                                        theme::text_subtle(),
                                    )),
                            )
                            .child(
                                kit::answer_text()
                                    .line_clamp(3)
                                    .child(first_lines(&m.memory.body, &m.memory.title)),
                            ),
                    )
                    .into_any_element(),
            ),
            Card::Answer => {
                let a = self.answer.as_ref();
                let text = a.map(|a| a.text.clone()).unwrap_or_default();
                let done = a.is_some_and(|a| a.done);
                let error = a.and_then(|a| a.error.clone());
                let sources = a.map(|a| a.sources.clone()).unwrap_or_default();
                let read = describe_sources(&sources);
                (
                    Tint::Neutral,
                    div()
                        .flex()
                        .flex_col()
                        .w_full()
                        .child(
                            kit::answer_header()
                                .child(icon("sparkles", 13.).text_color(theme::text_subtle()))
                                .child(format!("Quick answer · {}{read}", answer::MODEL_LABEL)),
                        )
                        .child(match error {
                            Some(e) => kit::answer_text()
                                .text_color(theme::text_subtle())
                                .child(e)
                                .into_any_element(),
                            // Claude finished without a word: say so, or the
                            // spinner below would turn forever.
                            None if done && text.is_empty() => kit::answer_text()
                                .text_color(theme::text_subtle())
                                .child("Claude gave no answer.")
                                .into_any_element(),
                            // Faint text alone read as an empty input box, so the
                            // wait gets the app's spinner and a label that breathes.
                            None if text.is_empty() => kit::answer_text()
                                .flex()
                                .items_center()
                                .gap(crate::ui::sp(8.))
                                .child(kit::spinner(13., theme::working()))
                                .child(div().child("Asking Claude…").with_animation(
                                    "answer-wait-label",
                                    Animation::new(Duration::from_millis(1400))
                                        .repeat_synced()
                                        .with_easing(gpui::pulsating_between(0.45, 1.)),
                                    |d, t| d.opacity(t),
                                ))
                                .into_any_element(),
                            None => kit::answer_text()
                                .child(answer_runs(&text))
                                .when(!done, |d| {
                                    d.child(
                                        div()
                                            .w(px(7.))
                                            .h(px(15.))
                                            .rounded(crate::ui::rad(1.))
                                            .bg(theme::active())
                                            .with_animation(
                                                "answer-cursor",
                                                Animation::new(Duration::from_millis(1000))
                                                    .repeat_synced(),
                                                |d, t| d.opacity(cursor_blink(t)),
                                            ),
                                    )
                                })
                                .into_any_element(),
                        })
                        .when(!sources.is_empty(), |d| {
                            d.child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .gap(crate::ui::sp(6.))
                                    .mt(crate::ui::sp(12.))
                                    .children(
                                        sources
                                            .iter()
                                            .map(|(ic, l)| kit::source_chip(ic, l.clone())),
                                    ),
                            )
                        })
                        .into_any_element(),
                )
            }
            Card::Rule(d) => {
                let mut well = kit::rule_well().child(kit::rule_line(
                    "WHEN",
                    d.trigger.icon(),
                    d.trigger.describe(),
                ));
                for (n, s) in d.steps.iter().enumerate() {
                    well = well.child(kit::rule_line(
                        if n == 0 { "DO" } else { "THEN" },
                        s.icon(),
                        s.describe(),
                    ));
                }
                (
                    Tint::Violet,
                    div()
                        .flex()
                        .flex_col()
                        .w_full()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(crate::ui::sp(10.))
                                .child(kit::glow_tile(
                                    if matches!(d.trigger, rules::Trigger::At { .. }) { "alarm-clock" } else { "flash" },
                                    Tint::Violet,
                                ))
                                .child(kit::card_text(
                                    if matches!(d.trigger, rules::Trigger::At { .. }) {
                                        "Set a reminder"
                                    } else {
                                        "Create an automation"
                                    },
                                    Some(
                                        div()
                                            .child(match &d.trigger {
                                                rules::Trigger::At { at } => format!(
                                                    "Rings at {} on the island. Nothing is set until you confirm.",
                                                    rules::clock(*at)
                                                ),
                                                _ => "Understood as one rule. Nothing runs until you confirm.".into(),
                                            })
                                            .into_any_element(),
                                    ),
                                ))
                                .when(selected, |d| d.child(enter())),
                        )
                        .child(well)
                        .into_any_element(),
                )
            }
            Card::Todo(text) => (
                Tint::Green,
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .w_full()
                    .child(kit::glow_tile("check-list", Tint::Green))
                    .child(kit::card_text(
                        format!("Add to-do: {text}"),
                        Some(
                            div()
                                .child(if rules::timeless_reminder(&self.query(cx)).is_some() {
                                    "No time given, so it's kept as a to-do. Add “in 30 min” or “at 5pm” to make it ring."
                                } else {
                                    "Shows under To-do here; Enter on it ticks it off."
                                })
                                .into_any_element(),
                        ),
                    ))
                    .when(selected, |d| d.child(enter()))
                    .into_any_element(),
            ),
            Card::Cloud(task) => (Tint::Violet, self.render_cloud_card(task, selected, cx)),
            Card::Confirm(item, title, head) => (
                Tint::Amber,
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .w_full()
                    .child(kit::glow_tile("git-merge", Tint::Amber))
                    .child(kit::card_text(
                        format!("Merge #{}: {title}?", item.number),
                        Some(
                            div()
                                .child(format!(
                                    "Squash and merge {} on GitHub ({}). Enter merges, Esc cancels.",
                                    super::short_head(head),
                                    item.repo
                                ))
                                .into_any_element(),
                        ),
                    ))
                    .child(kit::keycap("↵"))
                    .into_any_element(),
            ),
        };
        let key = self.entries[i].item.key.clone();
        kit::palette_card(
            ElementId::NamedInteger("palette-card".into(), i as u64),
            self.entries[i].item.title.clone(),
            tint,
            selected,
        )
        .when(
            matches!(
                card,
                Card::Answer | Card::Rule(_) | Card::Remember(_) | Card::Cloud(_)
            ),
            |d| d.items_start(),
        )
        .when(selected, |d| d.child(bar(&key)))
        .child(body)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.selected = i;
            this.pick(i, cx);
        }))
        .into_any_element()
    }

    /// The cloud task's form: what the agent does, and where it lands (the
    /// picked pull request or a new branch, whose name can be edited).
    fn render_cloud_card(&self, task: &str, selected: bool, cx: &mut Context<Self>) -> AnyElement {
        let pilot = self.seed.pilot.clone().unwrap_or_default();
        let to_pull = self.cloud_to_pull && pilot.pull.is_some();
        let (title, sub) = if task.is_empty() {
            (
                "Send to the cloud agent".to_string(),
                "Write the task after “cloud:”.".to_string(),
            )
        } else {
            (
                cloud::title(task),
                "For the cloud agent (arakun-pilot)".to_string(),
            )
        };
        let mut targets = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(crate::ui::sp(6.))
            .child(caption("Goes to", theme::text_subtle()));
        if let Some((n, t)) = &pilot.pull {
            targets = targets.child(
                kit::chip(
                    "palette-cloud-pull",
                    format!("PR #{n} · {}", cloud::short(t, 44)),
                    to_pull,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.set_cloud_target(true, cx);
                })),
            );
        }
        targets = targets.child(
            kit::chip("palette-cloud-branch", "New branch", !to_pull).on_click(cx.listener(
                |this, _, _, cx| {
                    cx.stop_propagation();
                    this.set_cloud_target(false, cx);
                },
            )),
        );
        let branch = (!to_pull).then(|| {
            div()
                .id("palette-cloud-branch-field")
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                // A click in the field edits the name; it doesn't send.
                .on_click(cx.listener(|_, _, _, cx| cx.stop_propagation()))
                .child(
                    kit::field()
                        .flex_1()
                        .min_w_0()
                        .child(icon("git-branch", 13.).text_color(theme::text_subtle()))
                        .child(div().flex_1().min_w_0().child(self.cloud_branch.clone())),
                )
                .child(caption("from main", theme::text_subtle()))
        });
        div()
            .flex()
            .items_start()
            .gap(crate::ui::sp(12.))
            .w_full()
            .child(kit::glow_tile("cloud-upload", Tint::Violet))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(crate::ui::sp(8.))
                    .child(kit::card_text(
                        title,
                        Some(div().child(sub).into_any_element()),
                    ))
                    .child(targets)
                    .children(branch)
                    .when(!pilot.admin, |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(crate::ui::sp(6.))
                                .child(icon("alert-circle", 13.).text_color(theme::attention()))
                                .child(caption(
                                    crate::github::pilot::NO_ADMIN,
                                    theme::attention().into(),
                                )),
                        )
                    }),
            )
            .when(selected, |d| d.child(kit::keycap("↵")))
            .into_any_element()
    }

    fn render_pins(&self, cx: &mut Context<Self>) -> AnyElement {
        let n = self.pins.items.len();
        div()
            .flex()
            .flex_col()
            .child(
                kit::palette_section(
                    "Pinned",
                    None,
                    self.rows.first().is_some_and(|r| matches!(r, Row::Pins)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(4.))
                        .child(kit::keycaps(&["Ctrl", "1"]))
                        .when(n > 1, |d| {
                            d.child("…").child(kit::keycaps(&["Ctrl", last_pin_key(n)]))
                        }),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(crate::ui::sp(8.))
                    .pt(crate::ui::sp(2.))
                    .px(crate::ui::sp(4.))
                    .pb(crate::ui::sp(6.))
                    .children(self.pins.items.iter().enumerate().map(|(i, p)| {
                        let item = self.seed.items.iter().find(|it| it.key == p.key());
                        let (ic, tint) = match item.map(|it| &it.lead) {
                            Some(Lead::Icon(ic, t)) => (*ic, *t),
                            Some(Lead::Working | Lead::Idle | Lead::Dot(..)) => {
                                ("message01", Tint::Green)
                            }
                            Some(Lead::Avatar(..)) => ("user", Tint::Neutral),
                            None if p.kind == "file" => ("folder01", Tint::Blue),
                            None => ("pin", Tint::Neutral),
                        };
                        kit::pin_chip(
                            ElementId::NamedInteger("palette-pin".into(), i as u64),
                            ic,
                            tint,
                            p.label.clone(),
                            i + 1,
                            false,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.open_pinned(i, cx)))
                    })),
            )
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut first = true;
        let rows: Vec<AnyElement> = self
            .rows
            .iter()
            .map(|r| {
                let el = match r {
                    Row::Section { label, count } => {
                        kit::palette_section(label.clone(), count.clone(), first).into_any_element()
                    }
                    Row::Pins => self.render_pins(cx),
                    Row::Entry(i) => self.render_row(*i, cx),
                };
                first = false;
                el
            })
            .collect();
        let empty = self.entries.is_empty() && !self.rows.iter().any(|r| matches!(r, Row::Pins));
        div()
            .id("palette-list")
            .role(gpui::Role::ListBox)
            .aria_label("Results")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .pt(crate::ui::sp(8.))
            .px(crate::ui::sp(kit::PALETTE_LIST_PAD))
            .pb(crate::ui::sp(10.))
            .children(rows)
            .when(empty, |d| {
                d.child(kit::palette_note(
                    if self.files.is_none() && self.scope == Some(Scope::Files) {
                        "Reading the project's files…".to_string()
                    } else {
                        crate::platform::shortcut_label(
                            "Nothing matches. Ctrl+Enter asks an agent instead.",
                        )
                    },
                ))
            })
            .into_any_element()
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let f = kit::palette_footer();
        let lead = self.entries.first().and_then(|e| e.card.clone());
        let right = |text: String| div().ml_auto().child(text);
        // What just happened (copied, pinned, refused) takes the footer's right side.
        if let Some((text, ok)) = &self.note {
            return f
                .child(kit::footer_hint(&["↑", "↓"], "move"))
                .child(kit::footer_hint(&["↵"], "open"))
                .child(kit::footer_hint(&["→"], "actions"))
                .child(
                    div()
                        .ml_auto()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .min_w_0()
                        .text_color(if *ok {
                            theme::text_subtle()
                        } else {
                            theme::attention().into()
                        })
                        .child(
                            icon(if *ok { "tick02" } else { "alert-circle" }, 13.).text_color(
                                if *ok {
                                    theme::success()
                                } else {
                                    theme::attention()
                                },
                            ),
                        )
                        .child(div().truncate().child(text.clone())),
                )
                .into_any_element();
        }
        if !self.marked.is_empty() {
            return f
                .child(kit::footer_hint(&["Space"], "select"))
                .child(kit::footer_hint(&["Ctrl", "A"], "all"))
                .child(kit::footer_hint(&["Esc"], "clear"))
                .child(right(format!(
                    "{} of {} selected",
                    self.marked.len(),
                    self.entries.iter().filter(|e| e.card.is_none()).count()
                )))
                .into_any_element();
        }
        if let Some(menu) = &self.menu {
            return f
                .child(kit::footer_hint(&["↑", "↓"], "move"))
                .child(kit::footer_hint(&["↵"], "run"))
                .child(kit::footer_hint(&["←"], "back"))
                .child(right(plural(menu.actions.len(), "action")))
                .into_any_element();
        }
        match lead {
            Some(Card::Answer) => {
                return f
                    .child(kit::footer_hint(&["Tab"], "keep chatting"))
                    .child(kit::footer_hint(&["↵"], "run"))
                    .when(
                        self.answer.as_ref().is_some_and(|a| !a.text.is_empty()),
                        |d| d.child(kit::footer_hint(&["Ctrl", "C"], "copy answer")),
                    )
                    .when_some(
                        self.answer.as_ref().filter(|a| a.done && a.tokens.1 > 0),
                        |d, a| {
                            d.child(right(format!(
                                "{} · {} tokens",
                                answer::MODEL_LABEL,
                                a.tokens.0 + a.tokens.1
                            )))
                        },
                    )
                    .into_any_element();
            }
            Some(Card::Rule(_) | Card::Todo(_)) => {
                return f
                    .child(kit::footer_hint(&["↵"], "confirm"))
                    .child(kit::footer_hint(&["Esc"], "cancel"))
                    .child(right("Nothing runs until you confirm".into()))
                    .into_any_element();
            }
            Some(Card::OfferAnswer | Card::OfferRule)
                if self.selected_entry().is_some_and(|e| e.card == lead) =>
            {
                return f
                    .child(kit::footer_hint(
                        &["↵"],
                        if lead == Some(Card::OfferAnswer) {
                            "ask Claude"
                        } else {
                            "read as an automation"
                        },
                    ))
                    .child(kit::footer_hint(&["↑", "↓"], "move"))
                    .child(kit::footer_hint(&["Ctrl", "↵"], "ask an agent"))
                    .child(right("Nothing is sent until you press Enter".into()))
                    .into_any_element();
            }
            Some(Card::Cloud(_)) => {
                return f
                    .child(kit::footer_hint(&["↵"], "send"))
                    .when(self.cloud_pull().is_some(), |d| {
                        d.child(kit::footer_hint(&["Tab"], "PR or new branch"))
                    })
                    .child(kit::footer_hint(&["Esc"], "cancel"))
                    .child(right("Nothing goes until you press Enter".into()))
                    .into_any_element();
            }
            Some(Card::Confirm(..)) => {
                return f
                    .child(kit::footer_hint(&["↵"], "merge"))
                    .child(kit::footer_hint(&["Esc"], "cancel"))
                    .into_any_element();
            }
            _ => {}
        }
        if !self.pages.is_empty() {
            return f
                .child(kit::footer_hint(&["↑", "↓"], "move"))
                .child(kit::footer_hint(&["→"], "go in"))
                .child(kit::footer_hint(&["←"], "back"))
                .child(kit::footer_hint(&["⌫"], "up one level"))
                .child(right(plural(self.entries.len(), "item")))
                .into_any_element();
        }
        if self.scope == Some(Scope::Settings) {
            return f
                .child(kit::footer_hint(&["↑", "↓"], "move"))
                .child(kit::footer_hint(&["↵"], "toggle"))
                .child(right(plural(self.entries.len(), "setting")))
                .into_any_element();
        }
        let q = self.query_text(cx);
        let pinnable = self
            .entries
            .get(self.selected)
            .is_some_and(|e| e.card.is_none());
        f.child(kit::footer_hint(&["↑", "↓"], "move"))
            .child(kit::footer_hint(&["↵"], "open"))
            .child(kit::footer_hint(&["→"], "actions"))
            .when(self.tabs.len() > 1, |d| {
                d.child(kit::footer_hint(&["Tab"], "filter"))
            })
            .when(pinnable && q.is_empty(), |d| {
                d.child(kit::footer_hint(&["Alt", "P"], "pin"))
            })
            .child(
                div()
                    .ml_auto()
                    .child(kit::footer_hint(&["Ctrl", "↵"], "ask an agent")),
            )
            .into_any_element()
    }

    fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        // Beside the selected row: rows sit 38 apart under the list's top.
        let row = self
            .rows
            .iter()
            .position(|r| matches!(r, Row::Entry(i) if *i == self.selected))
            .unwrap_or(0);
        let _ = row;
        Some(
            div()
                .id("palette-actions")
                .occlude()
                .absolute()
                // Clear of the preview and of the footer, whose padding
                // scales with the spacing while its text line doesn't.
                .right(px(kit::PALETTE_PREVIEW_W) + crate::ui::sp(16.))
                .bottom(px(19.) + crate::ui::sp(39.))
                .w(px(290.))
                .p(crate::ui::sp(6.))
                .flex()
                .flex_col()
                .gap(crate::ui::sp(2.))
                .rounded(crate::ui::rad(12.))
                .bg(theme::surface(0.16))
                .border_1()
                .border_color(theme::border())
                .shadow_xl()
                .child(
                    kit::palette_section(format!("Actions · {}", menu.title), None, true)
                        .px(crate::ui::sp(8.))
                        .child(kit::keycap("→")),
                )
                .children(menu.actions.iter().enumerate().map(|(n, a)| {
                    let on = n == menu.selected;
                    let what = a.what.clone();
                    let key = menu.key.clone();
                    kit::palette_row(
                        ElementId::NamedInteger("palette-act".into(), n as u64),
                        a.label.clone(),
                        on,
                    )
                    // 7 px (padding and border) inside the menu's 12 px corner.
                    .rounded(crate::ui::nested(12., 7.))
                    .h(px(40.))
                    .px(crate::ui::sp(8.))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(26.))
                            .rounded(crate::ui::rad(8.))
                            .bg(theme::fill_raised())
                            .child(icon(a.icon, 14.).text_color(theme::text_muted())),
                    )
                    .child(kit::row_title(a.label.clone(), &[], on, false))
                    .child(kit::row_end(on).child(kit::keycap(if on {
                        "↵".to_string()
                    } else {
                        a.key.to_ascii_uppercase().to_string()
                    })))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.run_action(&key, what.clone(), cx);
                        cx.notify();
                    }))
                }))
                .into_any_element(),
        )
    }

    fn render_bulk(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.marked.is_empty() {
            return None;
        }
        let kinds = self.marked_kinds();
        let sessions = kinds.iter().all(|k| *k == Kind::Session);
        let mut bar = kit::bulk_bar().child(
            div()
                .mr(crate::ui::sp(8.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(format!("{} selected", self.marked.len())),
        );
        let mut buttons: Vec<(&'static str, Option<&'static str>, &'static str, Bulk)> = Vec::new();
        if sessions {
            buttons.push(("bulk-sum", Some("note01"), "Summarize", Bulk::Summarize));
        }
        buttons.push(("bulk-pin", Some("pin"), "Pin", Bulk::Pin));
        buttons.push(("bulk-copy", Some("copy01"), "Copy", Bulk::Copy));
        buttons.push(("bulk-open", None, "Open all", Bulk::Open));
        let last = buttons.len() - 1;
        for (n, (id, ic, label, op)) in buttons.into_iter().enumerate() {
            bar = bar.child(
                kit::bulk_button(id, ic, label, n == last)
                    .on_click(cx.listener(move |this, _, _, cx| this.bulk(op, cx))),
            );
        }
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(58.))
                .flex()
                .justify_center()
                .child(bar)
                .into_any_element(),
        )
    }
}

fn short_url(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string()
}

/// What a memory card says: its text, cut at a word to fit three lines.
fn first_lines(body: &str, title: &str) -> String {
    let b = body.trim();
    if b.is_empty() {
        return title.to_string();
    }
    if b.chars().count() <= 230 {
        return b.to_string();
    }
    let cut: String = b.chars().take(230).collect();
    let at = cut.rfind(' ').unwrap_or(cut.len());
    format!("{}…", cut[..at].trim_end_matches([',', '.', ';', ':']))
}

/// `· read 2 runs and 1 memory`.
fn describe_sources(sources: &[(&'static str, String)]) -> String {
    let runs = sources.iter().filter(|(i, _)| *i == "activity01").count();
    let mems = sources.iter().filter(|(i, _)| *i == "bot").count();
    let mut parts = Vec::new();
    if runs > 0 {
        parts.push(format!("{runs} run{}", if runs == 1 { "" } else { "s" }));
    }
    if mems > 0 {
        parts.push(format!(
            "{mems} memor{}",
            if mems == 1 { "y" } else { "ies" }
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" · read {}", parts.join(" and "))
    }
}

/// The answer with `code` in mono on a soft ground.
fn answer_runs(text: &str) -> gpui::StyledText {
    let mut plain = String::new();
    let mut runs: Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> = Vec::new();
    let mut in_code = false;
    let mut start = 0;
    for c in text.chars() {
        if c == '`' {
            if in_code {
                runs.push((
                    start..plain.len(),
                    gpui::HighlightStyle {
                        color: Some(theme::text()),
                        background_color: Some(theme::fill_selected()),
                        ..Default::default()
                    },
                ));
            } else {
                start = plain.len();
            }
            in_code = !in_code;
            continue;
        }
        plain.push(c);
    }
    gpui::StyledText::new(SharedString::from(plain)).with_highlights(runs)
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let preview = self.render_preview(cx);
        let menu = self.render_menu(cx);
        let bulk = self.render_bulk(cx);
        let tabs = self.render_tabs(cx);
        let tabs_shown = tabs.is_some();
        kit::palette_panel("palette")
            .key_context("Palette")
            .capture_action(cx.listener(|this, _: &Right, _, cx| {
                if this.right(cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &Left, _, cx| {
                if this.left(cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &Backspace, _, cx| {
                if this.backspace_empty(cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &SelectAll, _, cx| {
                if this.mark_all(cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &CopyText, _, cx| {
                if !this.input.read(cx).has_selection() && this.copy_answer(cx) {
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &MarkRow, _, cx| {
                if !this.mark(cx) {
                    this.input.update(cx, |i, cx| i.insert(" ", cx));
                }
            }))
            .on_action(cx.listener(|this, _: &TogglePin, _, cx| {
                if let Some(e) = this.selected_entry().cloned() {
                    this.toggle_pin(&e.item, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PinUp, _, cx| this.nudge_pin(-1, cx)))
            .on_action(cx.listener(|this, _: &PinDown, _, cx| this.nudge_pin(1, cx)))
            .on_action(cx.listener(|this, _: &Pinned1, _, cx| this.open_pinned(0, cx)))
            .on_action(cx.listener(|this, _: &Pinned2, _, cx| this.open_pinned(1, cx)))
            .on_action(cx.listener(|this, _: &Pinned3, _, cx| this.open_pinned(2, cx)))
            .on_action(cx.listener(|this, _: &Pinned4, _, cx| this.open_pinned(3, cx)))
            .on_action(cx.listener(|this, _: &Pinned5, _, cx| this.open_pinned(4, cx)))
            .on_action(cx.listener(|this, _: &Pinned6, _, cx| this.open_pinned(5, cx)))
            .on_action(cx.listener(|this, _: &Pinned7, _, cx| this.open_pinned(6, cx)))
            .on_action(cx.listener(|this, _: &Pinned8, _, cx| this.open_pinned(7, cx)))
            .on_action(cx.listener(|this, _: &Pinned9, _, cx| this.open_pinned(8, cx)))
            .child(self.render_query(cx))
            .when_some(tabs, |d, t| d.child(t))
            .when(!tabs_shown, |d| {
                d.child(div().flex_none().h(px(1.)).bg(theme::border_subtle()))
            })
            .child(
                div()
                    .flex()
                    .flex_shrink_1()
                    .min_h_0()
                    .h(px(kit::PALETTE_BODY_H))
                    .child(self.render_list(cx))
                    .child(preview),
            )
            .child(self.render_footer(cx))
            .children(menu)
            .children(bulk)
    }
}

/// The answer cursor's opacity `t` into a blink: it holds, then dims and
/// comes back smoothly, where a hard on and off read as flicker.
fn cursor_blink(t: f32) -> f32 {
    const HOLD: f32 = 0.45;
    if t < HOLD {
        1.
    } else {
        let dip = (std::f32::consts::PI * (t - HOLD) / (1. - HOLD)).sin();
        1. - 0.75 * dip * dip
    }
}

/// The key of the last pin with one: a hand-edited pins.json can hold
/// more than nine, and only the first nine have a key.
fn last_pin_key(pins: usize) -> &'static str {
    ["1", "2", "3", "4", "5", "6", "7", "8", "9"][pins.clamp(1, 9) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn more_than_nine_pins_show_the_ninth_key() {
        assert_eq!(last_pin_key(2), "2");
        assert_eq!(last_pin_key(9), "9");
        assert_eq!(last_pin_key(12), "9");
    }
}
