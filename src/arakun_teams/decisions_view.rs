//! Decisions: what the team accepted from @bot, read back from every
//! channel's history, with who accepted it, when, and where it was argued.

use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::super::decisions::Decision;
use super::super::hub;
use super::bot_view::names;
use super::{Faces, Pane, TeamsView, clock, day_label};
use crate::theme;
use crate::ui::{self, ButtonKind, Type, Typography as _, icon};

/// The panel a decision opens in, at the right.
const DECISION_W: f32 = 340.;

/// "today, 10:58" for today, "Nov 14" before.
fn short_when(at: u64) -> String {
    let day = day_label(at);
    if day == "Today" {
        format!("today, {}", clock(at))
    } else {
        day
    }
}

/// "23 messages in protocol".
fn source_of(d: &Decision, channel: &str) -> String {
    match d.read {
        Some(1) => format!("1 message in {channel}"),
        Some(n) => format!("{n} messages in {channel}"),
        None => format!("messages in {channel}"),
    }
}

/// Whether a search finds the decision: in what was decided, who took part,
/// what @bot checked, or the channel.
fn finds(d: &Decision, channel: &str, query: &str) -> bool {
    let q = query.to_lowercase();
    let mut hay = vec![d.decision.clone(), d.via_login.clone(), channel.to_string()];
    hay.extend(d.accepted.iter().map(|a| a.login.clone()));
    hay.extend(
        d.verified
            .iter()
            .flat_map(|v| [v.source.clone(), v.text.clone()]),
    );
    hay.iter().any(|h| h.to_lowercase().contains(&q))
}

impl TeamsView {
    /// Opens every channel of the team, so the log reads all of them.
    pub(super) fn open_every_channel(&mut self, team_id: &str, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .hub
            .read(cx)
            .team
            .get(team_id)
            .map(|t| t.channels.iter().map(|c| c.id.clone()).collect())
            .unwrap_or_default();
        self.hub.update(cx, |h, cx| {
            for c in &ids {
                h.open_channel(team_id, c, cx);
            }
        });
    }

    /// Opens the channel a decision came from at the first message @bot read.
    fn open_source(&mut self, d: &Decision, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(team) = self.selected_team().map(str::to_string) else {
            return;
        };
        self.pending_jump = Some(d.range[0].clone());
        self.show(Pane::Channel(team, d.channel_id.clone()), window, cx);
    }

    pub(super) fn render_decisions(
        &mut self,
        team_id: &str,
        faces: &Faces,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hub = self.hub.read(cx);
        let me = hub.me().map_or(0, |m| m.github_id);
        let log = hub.decision_log(team_id);
        let channel_names: HashMap<String, String> = hub
            .team
            .get(team_id)
            .map(|t| {
                t.channels
                    .iter()
                    .map(|c| {
                        (
                            c.id.clone(),
                            c.name.clone().unwrap_or_else(|| "a locked channel".into()),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let reading = hub.team.get(team_id).is_none_or(|t| {
            t.channels.iter().any(|c| {
                hub.channels
                    .get(&hub::channel_key(team_id, &c.id))
                    .is_none_or(|s| s.loading)
            })
        });
        let channel = |id: &str| channel_names.get(id).cloned().unwrap_or_default();
        let query = self.decision_query.read(cx).text().trim().to_string();
        let mine = self.decisions_mine;
        let shown: Vec<&Decision> = log
            .iter()
            .filter(|d| !mine || d.via == me || d.accepted_by(me))
            .filter(|d| query.is_empty() || finds(d, &channel(&d.channel_id), &query))
            .collect();
        let picked = self
            .decision
            .as_ref()
            .and_then(|id| log.iter().find(|d| d.bot_id == *id))
            .cloned();

        let filters = ui::segmented()
            .child(
                ui::segment("teams-decisions-all", None, "All", !mine).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.decisions_mine = false;
                        cx.notify();
                    },
                )),
            )
            .child(
                ui::segment("teams-decisions-mine", None, "Mine", mine).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.decisions_mine = true;
                        cx.notify();
                    },
                )),
            );
        let search = div()
            .flex()
            .gap(ui::sp(8.))
            .child(
                ui::field()
                    .flex_1()
                    .min_w_0()
                    .child(icon("search01", 14.).text_color(theme::text_subtle()))
                    .child(div().flex_1().min_w_0().child(self.decision_query.clone())),
            )
            .child(filters);

        let list: AnyElement = if shown.is_empty() {
            if reading && log.is_empty() {
                ui::loading("teams-decisions-reading", "Reading the team's channels…")
                    .into_any_element()
            } else if log.is_empty() {
                ui::empty_state(
                    "tick-double02",
                    "No decisions yet",
                    "Reply to a message with @bot. When someone accepts the decision it proposes, it shows up here.",
                    None,
                )
                .into_any_element()
            } else {
                ui::text(
                    Type::Body,
                    theme::text_subtle(),
                    "No decision matches. Try a file, a word or a login.",
                )
                .px(ui::sp(4.))
                .into_any_element()
            }
        } else {
            let mut rows = Self::rows();
            for (i, d) in shown.iter().enumerate() {
                let on = picked.as_ref().is_some_and(|p| p.bot_id == d.bot_id);
                let who: Vec<String> = d.accepted.iter().map(|a| a.login.clone()).collect();
                let note = format!(
                    "Accepted by {} · {} · {}",
                    names(&who),
                    short_when(d.decided_at()),
                    source_of(d, &channel(&d.channel_id))
                );
                let id = d.bot_id.clone();
                rows = rows.child(
                    Self::person_row(
                        icon("tick-double02", 16.).text_color(theme::text_muted()),
                        d.title.clone(),
                        note,
                        on,
                    )
                    .id(("teams-decision", i))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::fill_hover()))
                    .when(on, |r| r.child(Self::outline_pill("Open")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.decision = if this.decision.as_deref() == Some(id.as_str()) {
                            None
                        } else {
                            Some(id.clone())
                        };
                        cx.notify();
                    })),
                );
            }
            Self::card().child(rows).into_any_element()
        };

        let page = div()
            .id("teams-decisions-page")
            .flex()
            .flex_col()
            .gap(ui::sp(16.))
            .flex_1()
            .min_w_0()
            .p(ui::sp(24.))
            .overflow_y_scroll()
            .child(ui::page_title(
                "Decisions",
                Some(
                    "What the team accepted, who accepted it, and where it was argued. Read this before you change something."
                        .into_any_element(),
                ),
            ))
            .child(search)
            .child(list);

        let side = picked.map(|d| {
            let from = source_of(&d, &channel(&d.channel_id));
            self.render_decision(d, from, faces, cx)
        });
        let column = self.render_column(team_id, "", faces, cx);
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .child(column)
            .child(page)
            .children(side)
            .into_any_element()
    }

    /// The panel of one decision: what it says, who accepted it and when,
    /// where it came from, and what @bot checked.
    fn render_decision(
        &self,
        d: Decision,
        from: String,
        faces: &Faces,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let who: Vec<String> = d.accepted.iter().map(|a| a.login.clone()).collect();
        let when = format!("{}, {}", day_label(d.decided_at()), clock(d.decided_at()));
        let mut sources: Vec<String> = Vec::new();
        for v in &d.verified {
            if !sources.contains(&v.source) {
                sources.push(v.source.clone());
            }
        }
        let (d1, d2) = (d.clone(), d.clone());
        let checked: AnyElement = if sources.is_empty() {
            ui::text(
                Type::Label,
                theme::text_subtle(),
                "@bot checked nothing in code or on the web for this one.",
            )
            .font_weight(FontWeight::NORMAL)
            .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap(ui::sp(6.))
                .child(ui::overline("What @bot checked"))
                .children(sources.into_iter().map(Self::bullet))
                .into_any_element()
        };
        div()
            .id("teams-decision-panel")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(DECISION_W))
            .border_l_1()
            .border_color(theme::border_subtle())
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .h(px(ui::BAR_H))
                    .pl(ui::sp(16.))
                    .pr(ui::sp(8.))
                    .child(
                        div()
                            .ty(Type::Body)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("Decision"),
                    )
                    .child(
                        ui::icon_button("teams-decision-close", "cancel01", 14.)
                            .tooltip(ui::tip("Close"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.decision = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(16.))
                    .px(ui::sp(16.))
                    .pt(ui::sp(8.))
                    .pb(ui::sp(16.))
                    .child(ui::text(Type::Heading, theme::text(), d.title.clone()))
                    .child(
                        div()
                            .ty(Type::Body)
                            .line_height(px(20.))
                            .text_color(theme::text_muted())
                            .child(d.decision.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(Self::fact(
                                "Accepted by",
                                div()
                                    .id("teams-decision-who")
                                    .flex()
                                    .justify_end()
                                    .gap(ui::sp(4.))
                                    .tooltip(ui::tip_text(names(&who)))
                                    .children(who.iter().map(|l| faces.avatar(l, 20.))),
                            ))
                            .child(Self::fact("When", when))
                            .child(Self::fact(
                                "From",
                                div()
                                    .id("teams-decision-from")
                                    .cursor_pointer()
                                    .text_color(theme::link())
                                    .hover(|s| s.underline())
                                    .child(from)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_source(&d1, window, cx)
                                    })),
                            ))
                            .child(Self::fact(
                                "Proposed by",
                                SharedString::from(format!("@bot, via {}", d.via_login)),
                            )),
                    )
                    .child(checked)
                    .child(
                        div().flex().gap(ui::sp(8.)).child(
                            ui::button(
                                "teams-decision-open",
                                "Open the messages",
                                ButtonKind::Outline,
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| this.open_source(&d2, window, cx),
                            )),
                        ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arakun_teams::bot::Verified;
    use crate::arakun_teams::decisions::Acceptance;
    use std::collections::BTreeMap;

    fn decision() -> Decision {
        Decision {
            bot_id: "b1".into(),
            channel_id: "c1".into(),
            title: "Use A".into(),
            decision: "Use A. Fall back to B.".into(),
            proposed_at: 10,
            via: 1,
            via_login: "karlo".into(),
            range: ["m1".into(), "m5".into()],
            read: Some(23),
            parts: None,
            cites: BTreeMap::new(),
            verified: vec![Verified {
                text: "Endorsements".into(),
                source: "PROTOCOL.md".into(),
            }],
            accepted: vec![Acceptance {
                github_id: 2,
                login: "dani".into(),
                at: 11,
                message_id: "a1".into(),
            }],
        }
    }

    #[test]
    fn search_finds_words_people_files_and_channels() {
        let d = decision();
        for q in [
            "fall back",
            "DANI",
            "karlo",
            "protocol.md",
            "endorse",
            "protocol",
        ] {
            assert!(finds(&d, "protocol", q), "{q}");
        }
        assert!(!finds(&d, "protocol", "rik"));
    }

    #[test]
    fn the_source_says_how_many_messages_and_where() {
        let mut d = decision();
        assert_eq!(source_of(&d, "protocol"), "23 messages in protocol");
        d.read = Some(1);
        assert_eq!(source_of(&d, "app"), "1 message in app");
        d.read = None;
        assert_eq!(source_of(&d, "app"), "messages in app");
    }
}
