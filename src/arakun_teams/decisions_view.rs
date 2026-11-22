//! Decisions: what the team accepted from @bot, read back from every
//! channel's history, with who accepted it, when, and where it was argued.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::super::decisions::Decision;
use super::super::hub;
use super::super::socket;
use super::bot_view::names;
use super::{Faces, Pane, TeamsView, clock, day_label};
use crate::theme;
use crate::ui::{self, ButtonKind, Type, Typography as _, icon};
use gpui::Focusable as _;

/// Which decisions the log shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Filter {
    All,
    /// The ones this user accepted or proposed.
    Mine,
    Reversed,
}

impl Filter {
    fn shows(self, d: &Decision, me: u64) -> bool {
        match self {
            Filter::All => true,
            Filter::Mine => d.via == me || d.accepted_by(me),
            Filter::Reversed => d.reversed.is_some(),
        }
    }
}

/// A built decision log, with the team, the hub's message revision and the
/// number of the team's open channels it was built from.
pub(super) type BuiltLog = ((String, u64, usize), Rc<Vec<Decision>>);

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
/// what @bot checked, the channel, or who reversed it and why.
fn finds(d: &Decision, channel: &str, query: &str) -> bool {
    let q = query.to_lowercase();
    let mut hay = vec![d.decision.clone(), d.via_login.clone(), channel.to_string()];
    hay.extend(d.accepted.iter().map(|a| a.login.clone()));
    hay.extend(
        d.verified
            .iter()
            .flat_map(|v| [v.source.clone(), v.text.clone()]),
    );
    if let Some(r) = &d.reversed {
        hay.extend([r.login.clone(), r.reason.clone()]);
    }
    hay.iter().any(|h| h.to_lowercase().contains(&q))
}

/// Whether Decisions opens a channel, given whether it has a link (`None`:
/// it was never opened). Entering the pane dials every channel with no link.
/// A change while it is open dials only the ones never opened: a refused
/// channel has no link either, and would be dialed again on every change.
fn wants_open(linked: Option<bool>, only_new: bool) -> bool {
    match linked {
        None => true,
        Some(linked) => !linked && !only_new,
    }
}

/// The channels Decisions opened, without the one now shown.
fn links_to_close(
    opened: HashSet<(String, String)>,
    shown: Option<(String, String)>,
) -> Vec<(String, String)> {
    opened
        .into_iter()
        .filter(|k| Some(k) != shown.as_ref())
        .collect()
}

/// How far the log has read one channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    Done,
    Reading,
    /// Refused, or never opened (no network, or no membership yet): its
    /// decisions are not in the log.
    Failed,
}

/// `state` is the channel's `loading` and socket status, when it was opened.
fn read_of(state: Option<(bool, &socket::Status)>) -> Read {
    match state {
        None | Some((_, socket::Status::Refused(_))) => Read::Failed,
        Some((true, _)) => Read::Reading,
        Some((false, _)) => Read::Done,
    }
}

/// The line over the log while it does not hold every channel: the list
/// must not read as complete, nor as "No decisions yet".
fn read_note(reading: usize, unread: usize) -> Option<String> {
    let channels = |n: usize| if n == 1 { "channel" } else { "channels" };
    let reading = (reading > 0).then(|| format!("Still reading {reading} {}…", channels(reading)));
    let unread = (unread > 0).then(|| {
        format!(
            "{unread} {} could not be read, so decisions may be missing.",
            channels(unread)
        )
    });
    match (reading, unread) {
        (Some(r), Some(u)) => Some(format!("{r} {u}")),
        (r, u) => r.or(u),
    }
}

impl TeamsView {
    /// Opens every channel of the team, so the log reads all of them, and
    /// notes the ones it opened: leaving Decisions closes those again.
    /// `only_new` is for a change while the pane is open: see `wants_open`.
    pub(super) fn open_every_channel(
        &mut self,
        team_id: &str,
        only_new: bool,
        cx: &mut Context<Self>,
    ) {
        let hub = self.hub.read(cx);
        let linked = |c: &str| {
            hub.channels
                .get(&hub::channel_key(team_id, c))
                .map(|s| s.linked())
        };
        let ids: Vec<String> = hub
            .team
            .get(team_id)
            .map(|t| t.channels.iter().map(|c| c.id.clone()).collect())
            .unwrap_or_default();
        let ids: Vec<String> = ids
            .into_iter()
            .filter(|c| wants_open(linked(c), only_new))
            .collect();
        if ids.is_empty() {
            return;
        }
        self.hub.update(cx, |h, cx| {
            for c in &ids {
                h.open_channel(team_id, c, cx);
            }
        });
        let hub = self.hub.read(cx);
        for c in ids {
            let key = hub::channel_key(team_id, &c);
            if hub.channels.get(&key).is_some_and(|s| s.linked()) {
                self.decisions_opened.insert((team_id.to_string(), c));
            }
        }
    }

    /// Off Decisions, hangs up the channels it opened to read the log. The
    /// one being shown stays, and so do the ones the user had open before.
    pub(super) fn close_decision_links(&mut self, cx: &mut Context<Self>) {
        if self.decisions_opened.is_empty() || matches!(self.pane, Pane::Decisions(_)) {
            return;
        }
        let shown = match &self.pane {
            Pane::Channel(t, c) => Some((t.clone(), c.clone())),
            _ => None,
        };
        let close = links_to_close(std::mem::take(&mut self.decisions_opened), shown);
        self.hub.update(cx, |h, cx| {
            for (t, c) in &close {
                h.close_channel(t, c, cx);
            }
        });
    }

    /// Reverses the decision whose reason field shows, with what was typed.
    pub(super) fn reverse_picked(&mut self, cx: &mut Context<Self>) {
        let (Some(bot_id), Some(team)) = (
            self.reversing.clone(),
            self.selected_team().map(str::to_string),
        ) else {
            return;
        };
        let Some(channel) = self
            .hub
            .read(cx)
            .decision_log(&team)
            .into_iter()
            .find(|d| d.bot_id == bot_id)
            .map(|d| d.channel_id)
        else {
            return;
        };
        let reason = self.reverse_reason.read(cx).text().to_string();
        let sent = self.hub.update(cx, |h, cx| {
            h.reverse_decision(&team, &channel, &bot_id, reason, cx)
        });
        if sent {
            self.reversing = None;
            self.reverse_reason.update(cx, |i, cx| i.set_text("", cx));
        }
        cx.notify();
    }

    /// Opens the channel a decision came from at the first message @bot read.
    fn open_source(&mut self, d: &Decision, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let Some(team) = self.selected_team().map(str::to_string) else {
            return;
        };
        self.pending_jump = Some(d.range[0].clone());
        self.show(Pane::Channel(team, d.channel_id.clone()), window, cx);
    }

    /// The team's decision log, built again only when a channel's messages
    /// changed or a channel was opened or closed: building it reads every
    /// message of every channel, and the pane renders on each keystroke.
    fn built_log(&mut self, team_id: &str, cx: &mut Context<Self>) -> Rc<Vec<Decision>> {
        let hub = self.hub.read(cx);
        let channels = hub
            .channels
            .values()
            .filter(|c| c.team_id == team_id)
            .count();
        let key = (team_id.to_string(), hub.messages_rev, channels);
        if let Some((built, log)) = &self.decision_log
            && *built == key
        {
            return log.clone();
        }
        let log = Rc::new(hub.decision_log(team_id));
        self.decision_log = Some((key, log.clone()));
        log
    }

    pub(super) fn render_decisions(
        &mut self,
        team_id: &str,
        faces: &Faces,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let log = self.built_log(team_id, cx);
        let hub = self.hub.read(cx);
        let me = hub.me().map_or(0, |m| m.github_id);
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
        // The team itself still loading reads as one channel being read.
        let (reading, unread) = hub.team.get(team_id).map_or((1, 0), |t| {
            let count = |r: Read| {
                t.channels
                    .iter()
                    .filter(|c| {
                        let state = hub.channels.get(&hub::channel_key(team_id, &c.id));
                        read_of(state.map(|s| (s.loading, &s.status))) == r
                    })
                    .count()
            };
            (count(Read::Reading), count(Read::Failed))
        });
        let note = read_note(reading, unread);
        let channel = |id: &str| channel_names.get(id).cloned().unwrap_or_default();
        let query = self.decision_query.read(cx).text().trim().to_string();
        let filter = self.decisions_filter;
        let shown: Vec<&Decision> = log
            .iter()
            .filter(|d| filter.shows(d, me))
            .filter(|d| query.is_empty() || finds(d, &channel(&d.channel_id), &query))
            .collect();
        let picked = self
            .decision
            .as_ref()
            .and_then(|id| log.iter().find(|d| d.bot_id == *id))
            .cloned();

        let mut filters = ui::segmented();
        for (id, label, f) in [
            ("teams-decisions-all", "All", Filter::All),
            ("teams-decisions-mine", "Mine", Filter::Mine),
            ("teams-decisions-reversed", "Reversed", Filter::Reversed),
        ] {
            filters = filters.child(ui::segment(id, None, label, filter == f).on_click(
                cx.listener(move |this, _, _, cx| {
                    this.decisions_filter = f;
                    cx.notify();
                }),
            ));
        }
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
            if reading > 0 && log.is_empty() {
                ui::loading("Reading the team's channels…").into_any_element()
            } else if log.is_empty() {
                ui::empty_state(
                    "tick-double02",
                    "No decisions yet",
                    "Reply to a message with @bot. When someone accepts the decision it proposes, it shows up here.",
                    None,
                )
                .into_any_element()
            } else if reading {
                // What is missing may be in a channel not read yet.
                ui::loading("Reading the team's channels…").into_any_element()
            } else if filter == Filter::Reversed && query.is_empty() {
                ui::text(Type::Body, theme::text_subtle(), "Nothing was reversed.")
                    .px(ui::sp(4.))
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
                let note = match &d.reversed {
                    Some(r) => format!(
                        "Reversed by {} · {} · {}",
                        r.login,
                        short_when(r.at),
                        source_of(d, &channel(&d.channel_id))
                    ),
                    None => format!(
                        "Accepted by {} · {} · {}",
                        names(&who),
                        short_when(d.decided_at()),
                        source_of(d, &channel(&d.channel_id))
                    ),
                };
                let lead = if d.reversed.is_some() {
                    icon("undo", 16.).text_color(theme::attention())
                } else {
                    icon("tick-double02", 16.).text_color(theme::text_muted())
                };
                let id = d.bot_id.clone();
                rows = rows.child(
                    Self::person_row(lead, d.title.clone(), note, on)
                        .id(("teams-decision", i))
                        .map(ui::focusable)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::fill_hover()))
                        .when(on, |r| r.child(Self::outline_pill("Open")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.decision = if this.decision.as_deref() == Some(id.as_str()) {
                                None
                            } else {
                                Some(id.clone())
                            };
                            this.reversing = None;
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
            // Not over the spinner, which says the same.
            .children(
                note.filter(|_| !(reading > 0 && log.is_empty()))
                    .map(|n| ui::text(Type::Caption, theme::text_subtle(), n).px(ui::sp(4.))),
            )
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
        let reversed = d.reversed.clone();
        let asking = self.reversing.as_deref() == Some(d.bot_id.as_str());
        let sending = self.hub.read(cx).channels.values().any(|c| {
            c.messages.iter().any(|m| {
                m.delivery == hub::Delivery::Sending
                    && super::super::decisions::reverses(m) == Some(d.bot_id.as_str())
            })
        });
        // A reversal that was not sent stays here with why and a way to send
        // it again: this panel is where it was asked for.
        let refused = self.selected_team().and_then(|team| {
            let c = self
                .hub
                .read(cx)
                .channels
                .get(&hub::channel_key(team, &d.channel_id))?;
            let (id, why) = super::super::decisions::refused_reversal(&c.messages, &d.bot_id)?;
            Some((team.to_string(), id.to_string(), why.to_string()))
        });
        let (reverse_button, reverse_form): (Option<AnyElement>, Option<AnyElement>) =
            if reversed.is_some() || sending {
                (None, None)
            } else if let Some((team, id, why)) = refused {
                let channel = d.channel_id.clone();
                (
                    Some(
                        ui::button(
                            "teams-reverse-resend",
                            "Send again",
                            ButtonKind::Ghost,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.hub
                                .update(cx, |h, cx| h.resend(&team, &channel, &id, cx))
                        }))
                        .into_any_element(),
                    ),
                    Some(
                        ui::text(
                            Type::Caption,
                            theme::error().into(),
                            format!("The reversal was not sent: {why}"),
                        )
                        .into_any_element(),
                    ),
                )
            } else if asking {
                (None, Some(div()
                .flex()
                .flex_col()
                .gap(ui::sp(8.))
                .child(ui::overline("Reverse this decision"))
                .child(
                    ui::field()
                        .child(div().flex_1().min_w_0().child(self.reverse_reason.clone())),
                )
                .child(
                    div()
                        .flex()
                        .gap(ui::sp(8.))
                        .child(
                            ui::button("teams-reverse-send", "Reverse", ButtonKind::Primary, true)
                                .on_click(cx.listener(|this, _, _, cx| this.reverse_picked(cx))),
                        )
                        .child(
                            ui::button("teams-reverse-cancel", "Cancel", ButtonKind::Ghost, true)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.reversing = None;
                                    cx.notify();
                                })),
                        ),
                )
                .child(Self::footnote(
                    "Everyone in the team sees it as reversed, here and in the channel.",
                ))
                .into_any_element()))
            } else {
                let id = d.bot_id.clone();
                (
                    Some(
                        ui::button(
                            "teams-decision-reverse",
                            "Reverse it",
                            ButtonKind::Ghost,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.reversing = Some(id.clone());
                            let handle = this.reverse_reason.read(cx).focus_handle(cx);
                            window.focus(&handle, cx);
                            cx.notify();
                        }))
                        .into_any_element(),
                    ),
                    None,
                )
            };
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
                        ui::icon_button("teams-decision-close", "cancel01", 14., "Close").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.decision = None;
                                cx.notify();
                            }),
                        ),
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
                            ))
                            .children(reversed.as_ref().map(|r| {
                                Self::fact(
                                    "Reversed by",
                                    SharedString::from(format!(
                                        "{}, {}",
                                        r.login,
                                        short_when(r.at)
                                    )),
                                )
                            }))
                            .children(
                                reversed.as_ref().filter(|r| !r.reason.is_empty()).map(|r| {
                                    Self::fact("Why", SharedString::from(r.reason.clone()))
                                }),
                            ),
                    )
                    .child(checked)
                    .child(
                        div()
                            .flex()
                            .gap(ui::sp(8.))
                            .child(
                                ui::button(
                                    "teams-decision-open",
                                    "Open the messages",
                                    ButtonKind::Outline,
                                    true,
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| this.open_source(&d2, window, cx),
                                )),
                            )
                            .children(reverse_button),
                    )
                    .children(reverse_form),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_opens_and_closes_only_its_own_links() {
        // Entering the pane: everything with no link, a refused one too.
        assert!(wants_open(None, false));
        assert!(wants_open(Some(false), false));
        assert!(!wants_open(Some(true), false));
        // A change while it is open: only a channel never opened.
        assert!(wants_open(None, true));
        assert!(!wants_open(Some(false), true));
        assert!(!wants_open(Some(true), true));

        let key = |c: &str| ("team".to_string(), c.to_string());
        let opened: HashSet<_> = [key("a"), key("b")].into();
        assert_eq!(links_to_close(opened.clone(), Some(key("a"))), [key("b")]);
        assert_eq!(links_to_close(opened, None).len(), 2);
    }

    #[test]
    fn the_log_says_what_it_has_not_read() {
        use socket::Status;
        let refused = Status::Refused("This channel is not open to you.".into());
        assert_eq!(read_of(Some((true, &Status::Connecting))), Read::Reading);
        assert_eq!(read_of(Some((false, &Status::Connected))), Read::Done);
        // Refused clears `loading`: that is not a channel read to the end.
        assert_eq!(read_of(Some((false, &refused))), Read::Failed);
        // Never opened is not loading either, or the spinner would never end.
        assert_eq!(read_of(None), Read::Failed);

        assert_eq!(read_note(0, 0), None);
        assert_eq!(
            read_note(3, 0).as_deref(),
            Some("Still reading 3 channels…")
        );
        assert_eq!(
            read_note(1, 2).as_deref(),
            Some(
                "Still reading 1 channel… 2 channels could not be read, so decisions may be missing."
            )
        );
    }
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
            reversed: None,
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
        // Who reversed it and why are on the row, so a search finds them.
        let mut d = d;
        d.reversed = Some(crate::arakun_teams::decisions::Reversal {
            github_id: 3,
            login: "rik".into(),
            at: 20,
            reason: "Too slow on a phone".into(),
            message_id: "r1".into(),
        });
        assert!(finds(&d, "protocol", "RIK"));
        assert!(finds(&d, "protocol", "too slow"));
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
