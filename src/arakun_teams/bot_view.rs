//! @bot in a channel: the range it will read while it is being asked, its
//! runs on this device, its answer with verdicts and a proposed decision,
//! and the "Decided" line once members accept it.

use std::collections::{HashMap, HashSet};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Focusable as _, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::super::bot::{self, Answer, BotMeta, Range, RangeError, Run, RunState, Step, Verdict};
use super::super::decisions::{self, Acceptance, Reversal};
use super::super::hub::{Delivery, Msg};
use super::{AVATAR, Pane, TeamsView, clock};
use crate::agent::AgentKind as Agent;
use crate::theme;
use crate::ui::{self, ButtonKind, Type, Typography as _, icon};

/// What the open channel's list needs to know about @bot, worked out once
/// per render.
pub(super) struct BotState {
    /// Messages in the range @bot would read, while the composer asks it.
    pub draft: HashSet<String>,
    /// The line over the composer that says what @bot will read.
    pub hint: Option<AnyElement>,
    /// This device's runs in the channel.
    pub runs: Vec<Run>,
    /// Each message a run reads, by its number in that run's range.
    pub numbers: HashMap<String, usize>,
    /// Who accepted each @bot answer that someone accepted, by its ID.
    pub decided: HashMap<String, Vec<Acceptance>>,
    /// Answers this user's acceptance of is still on its way to the server.
    pub accepting: HashSet<String>,
    pub me: u64,
    pub my_login: String,
    /// Answers this user turned down on this device.
    pub rejected: HashSet<String>,
    /// Answers whose range holds a message edited after them, by answer ID.
    pub stale: HashMap<String, Stale>,
    /// Answers whose decision a member reversed, by answer ID.
    pub reversed: HashMap<String, Reversal>,
}

/// A message in an answer's range that was edited after the answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stale {
    /// Its number in the range.
    pub n: usize,
    pub login: String,
    pub id: String,
}

/// The first message of a bot answer's range edited after the answer.
pub(super) fn stale_of(messages: &[Msg], answer: &Msg) -> Option<Stale> {
    let meta = bot::meta(answer)?;
    let after = answer.seq?;
    let range = bot::select_range(messages, &meta.range[1]).ok()?;
    range.lines.iter().find_map(|l| {
        let m = messages.iter().find(|m| m.id == l.id)?;
        let e = m.edited.as_ref()?;
        // An edit still on its way is newer than anything stored.
        e.seq.is_none_or(|s| s > after).then(|| Stale {
            n: l.n,
            login: m.login.clone(),
            id: m.id.clone(),
        })
    })
}

/// "Karlo", "Karlo and Dani", "Karlo, Dani and Rik".
pub(super) fn names(logins: &[String]) -> String {
    match logins {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A reason as a sentence: a capital first and a full stop.
fn sentence(s: &str) -> String {
    let s = s.trim();
    let mut c = s.chars();
    let mut out = match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect::<String>(),
        None => return String::new(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

/// A failed run's title and sentence: how far it got when it ran in
/// parts, and a usage limit in plain words when it hit one.
fn failed_text(run: &Run, why: &str) -> (String, String) {
    let now = super::now();
    let minute = crate::platform::local_time(now).map_or(0, |t| t.hour * 60 + t.minute);
    let device = super::super::hub::device_name();
    failed_text_at((run.done, run.parts), why, &device, now, minute as u32)
}

fn failed_text_at(
    (done, parts): (usize, usize),
    why: &str,
    device: &str,
    now: u64,
    minute: u32,
) -> (String, String) {
    let title = if parts > 0 && done < parts {
        format!("@bot stopped after {done} of {parts} parts")
    } else {
        "@bot stopped".to_string()
    };
    let body = match bot::usage_limit(why, now, minute) {
        Some(limit) => {
            let which = if limit.weekly { "weekly" } else { "5 h" };
            let device = crate::redact::value(device);
            match limit.resets_at {
                Some(at) => format!(
                    "Claude Code on {device} hit its {which} limit. It resets {}. Nothing was decided.",
                    super::until_from(at, now)
                ),
                None => {
                    format!("Claude Code on {device} hit its {which} limit. Nothing was decided.")
                }
            }
        }
        None => format!("{} Nothing was decided.", sentence(why)),
    };
    (title, body)
}

/// "read 23 messages", "read 214 messages, in 4 parts".
fn read_note(read: Option<usize>, parts: Option<usize>) -> Option<String> {
    let n = read.filter(|n| *n > 0)?;
    let mut note = format!("read {n} message{}", if n == 1 { "" } else { "s" });
    if let Some(p) = parts.filter(|p| *p > 1) {
        note.push_str(&format!(", in {p} parts"));
    }
    Some(note)
}

/// Each message's number in the runs that read it, from the range each run
/// read when it started. A message two runs number differently gets none,
/// so a number never points at another run's citation.
fn run_numbers<'a>(ranges: impl IntoIterator<Item = &'a [String]>) -> HashMap<String, usize> {
    let mut numbers: HashMap<String, Option<usize>> = HashMap::new();
    for ids in ranges.into_iter().filter(|ids| ids.len() > 1) {
        for (i, id) in ids.iter().enumerate() {
            let n = i + 1;
            numbers
                .entry(id.clone())
                .and_modify(|had| {
                    if *had != Some(n) {
                        *had = None;
                    }
                })
                .or_insert(Some(n));
        }
    }
    numbers
        .into_iter()
        .filter_map(|(id, n)| Some((id, n?)))
        .collect()
}

/// What the composer holds after Ask for more detail: the question to
/// finish, or the draft the user was typing, now addressed to @bot.
fn more_detail_text(draft: &str) -> String {
    if draft.trim().is_empty() {
        "@bot more detail on ".into()
    } else if bot::mentions_bot(draft) {
        draft.to_string()
    } else {
        format!("@bot {}", draft.trim_start())
    }
}

impl TeamsView {
    pub(super) fn bot_state(
        &self,
        messages: &[Msg],
        team_id: &str,
        channel_id: &str,
        cx: &mut Context<Self>,
    ) -> BotState {
        let asking = bot::mentions_bot(self.composer.read(cx).text());
        let (draft, hint) = if asking {
            let range = bot::draft_range(messages, self.reply_to.as_deref());
            // The last line is the message being typed.
            let ids = range
                .as_ref()
                .map(|r| {
                    r.lines[..r.len().saturating_sub(1)]
                        .iter()
                        .map(|l| l.id.clone())
                        .collect()
                })
                .unwrap_or_default();
            (ids, Some(Self::bot_hint(&range)))
        } else {
            (HashSet::new(), None)
        };
        let hub = self.hub.read(cx);
        let runs: Vec<Run> = hub.bot.in_channel(team_id, channel_id).cloned().collect();
        let numbers = run_numbers(runs.iter().map(|r| r.ids.as_slice()));
        // Only an accepted message needs its meta read, and only once.
        let mut accepted = decisions::acceptances(messages);
        let decided: HashMap<String, Vec<Acceptance>> = messages
            .iter()
            .filter(|m| m.delivery == Delivery::Sent)
            .filter_map(|m| {
                let a = accepted.remove(m.id.as_str())?;
                bot::meta(m).map(|_| (m.id.clone(), a))
            })
            .collect();
        let stale: HashMap<String, Stale> = messages
            .iter()
            .filter(|m| m.body.kind == "bot")
            .filter_map(|m| Some((m.id.clone(), stale_of(messages, m)?)))
            .collect();
        let reversed: HashMap<String, Reversal> = messages
            .iter()
            .filter(|m| m.delivery == Delivery::Sent && bot::meta(m).is_some())
            .filter_map(|m| Some((m.id.clone(), decisions::reversal(messages, &m.id)?)))
            .collect();
        let me = hub.me();
        let accepting = messages
            .iter()
            .filter(|m| m.delivery == Delivery::Sending)
            .filter_map(decisions::accepting)
            .map(String::from)
            .collect();
        BotState {
            stale,
            reversed,
            draft,
            hint,
            runs,
            numbers,
            decided,
            accepting,
            me: me.map_or(0, |m| m.github_id),
            my_login: me.map(|m| m.login.clone()).unwrap_or_default(),
            rejected: hub.bot.rejected.clone(),
        }
    }

    /// A range of the list @bot reads: the accent at its edge.
    pub(super) fn in_range<E: gpui::Styled>(e: E) -> E {
        e.bg(theme::fill_raised())
            .border_l_2()
            .border_color(theme::accent())
    }

    /// A word in an outlined pill with a dot: "via Karlo".
    pub(super) fn outline_pill(text: impl Into<SharedString>) -> gpui::Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(ui::sp(6.))
            .h(px(22.))
            .px(ui::sp(8.))
            .rounded(ui::rad(ui::RADIUS))
            .border_1()
            .border_color(theme::border_strong())
            .ty(Type::Label)
            .text_color(theme::text())
            .child(div().size(px(5.)).rounded_full().bg(theme::text()))
            .child(text.into())
    }

    /// @bot's picture: Claude's mark on a tile, square where people are round.
    fn bot_avatar() -> gpui::Div {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(AVATAR))
            .mt(px(2.))
            .rounded(ui::rad(ui::RADIUS_SM))
            .bg(theme::fill_selected())
            .child(crate::shell::agent_icon(Agent::Claude, 14.))
    }

    /// "@bot will read 23 messages, from that one down to here".
    fn bot_hint(range: &Result<Range, RangeError>) -> AnyElement {
        let (bold, rest) = match range {
            Ok(r) if r.len() > 1 => (
                format!("@bot will read {} messages", r.len()),
                ", from that one down to here".to_string(),
            ),
            Ok(_) => (
                "@bot will read this message only".to_string(),
                ". Reply to an earlier one to give it a range.".to_string(),
            ),
            Err(e) => ("@bot can't read this range".to_string(), format!(": {e}")),
        };
        Self::above_row()
            .child(crate::shell::agent_icon(Agent::Claude, 12.))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .when(range.is_err(), |d| d.text_color(theme::attention()))
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::MEDIUM)
                            .child(bold),
                    )
                    .child(div().min_w_0().truncate().child(rest)),
            )
            .child(
                Self::outline_pill("Runs on your Claude Code, read-only")
                    .id("teams-bot-where")
                    .tooltip(ui::tip(
                        "Claude Code runs on this machine and sends these messages, and the code it reads, to its model service to answer. The Teams server sees only sealed messages",
                    )),
            )
            .into_any_element()
    }

    /// The composer's @bot button: starts the message with "@bot".
    pub(super) fn render_bot_chip(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("teams-bot")
            .map(ui::focusable)
            .flex()
            .flex_none()
            .items_center()
            .gap(ui::sp(6.))
            .h(px(ui::CONTROL_H))
            .px(ui::sp(8.))
            .rounded(ui::rad(ui::RADIUS))
            .ty(Type::Label)
            .text_color(theme::text())
            .cursor_pointer()
            .hover(|s| s.bg(theme::fill_hover()))
            .tooltip(ui::tip(
                "Ask @bot. Reply to a message first and it reads from there down",
            ))
            .child(crate::shell::agent_icon(Agent::Claude, 12.))
            .child("@bot")
            .on_click(cx.listener(|this, _, window, cx| {
                let text = this.composer.read(cx).text().to_string();
                if !bot::mentions_bot(&text) {
                    this.composer.update(cx, |i, cx| {
                        i.set_text(format!("@bot {}", text.trim_start()), cx)
                    });
                }
                let handle = this.composer.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    /// The line over a message from @bot: who ran it, how much it read, when.
    fn bot_header(via: &str, read: Option<String>, at: u64) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(ui::sp(8.))
            .child(ui::text(Type::Body, theme::content(), "@bot").font_weight(FontWeight::SEMIBOLD))
            .child(Self::outline_pill(format!("via {via}")))
            .children(read.map(|r| ui::text(Type::Caption, theme::text_faint(), r)))
            .child(
                ui::text(Type::Caption, theme::text_faint(), clock(at))
                    .font_family(theme::FONT_MONO),
            )
    }

    /// An @bot answer: its verdicts and the decision it proposes, or one
    /// line once this user accepted it.
    pub(super) fn render_bot_message(
        &self,
        index: usize,
        m: &Msg,
        bot: &BotState,
        index_of: &HashMap<&str, usize>,
        team_channel: (&str, &str),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let meta = bot::meta(m);
        let answer = meta.as_ref().and_then(|x| x.answer.clone());
        let accepted = bot.decided.get(&m.id).cloned().unwrap_or_default();
        let mine = accepted.iter().any(|a| a.github_id == bot.me);
        let folded = mine && answer.is_some() && !self.unfolded.contains(&m.id);
        let read = if folded {
            None
        } else {
            meta.as_ref().and_then(|x| read_note(x.read, x.parts))
        };
        let lit = self.flash.as_deref() == Some(m.id.as_str());
        let body: AnyElement = match (&meta, answer) {
            (None, _) => div()
                .flex()
                .flex_col()
                .gap(ui::sp(2.))
                .child(
                    div()
                        .ty(Type::Body)
                        .text_color(theme::text())
                        .child(m.body.text.clone()),
                )
                .child(ui::text(
                    Type::Caption,
                    theme::attention().into(),
                    "Not posted by the member it names as running @bot.",
                ))
                .into_any_element(),
            (Some(_), None) => div()
                .ty(Type::Body)
                .text_color(theme::text())
                .child(m.body.text.clone())
                .into_any_element(),
            (Some(_), Some(a)) if folded => {
                let id = m.id.clone();
                div()
                    .id(("teams-bot-unfold", index))
                    .map(ui::focusable)
                    .cursor_pointer()
                    .ty(Type::Body)
                    .text_color(theme::text())
                    .hover(|s| s.text_color(theme::content()))
                    .tooltip(ui::tip("Show what @bot checked"))
                    .child(format!("Proposed: {}", a.decision))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.unfolded.insert(id.clone());
                        cx.notify();
                    }))
                    .into_any_element()
            }
            (Some(meta), Some(a)) => {
                self.render_answer(index, m, meta, &a, mine, bot, index_of, team_channel, cx)
            }
        };
        div()
            .id(("teams-message", index))
            .flex()
            .items_start()
            .gap(ui::sp(12.))
            .px(ui::sp(16.))
            .pt(ui::sp(8.))
            .pb(ui::sp(6.))
            .when(lit, |d| d.bg(theme::fill_selected()))
            .child(Self::bot_avatar())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(2.))
                    .flex_1()
                    .min_w_0()
                    .child(Self::bot_header(&m.login, read, m.at))
                    .child(body)
                    .children(self.delivery(index, m, team_channel, cx)),
            )
            .into_any_element()
    }

    /// The verdict a chip shows, and its dot's color.
    fn verdict_chip(v: Verdict) -> gpui::Div {
        let (word, color) = match v {
            Verdict::Works => ("Works", theme::success()),
            Verdict::WorksWithConditions => ("Works with conditions", theme::attention()),
            Verdict::WontWork => ("Won’t work", theme::error()),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(ui::sp(6.))
            .h(px(20.))
            .px(ui::sp(6.))
            .rounded(ui::rad(ui::RADIUS_SM))
            .bg(theme::fill_selected())
            .ty(Type::Caption)
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text_muted())
            .child(div().size(px(6.)).rounded_full().bg(color))
            .child(word)
    }

    /// A point in a list: a small dot and its text.
    pub(super) fn bullet(text: impl IntoElement) -> gpui::Div {
        div()
            .flex()
            .gap(ui::sp(8.))
            .ty(Type::Label)
            .font_weight(FontWeight::NORMAL)
            .line_height(px(18.))
            .text_color(theme::text_muted())
            .child(
                div()
                    .flex_none()
                    .size(px(4.))
                    .mt(px(7.))
                    .rounded_full()
                    .bg(theme::text_faint()),
            )
            .child(div().flex_1().min_w_0().child(text))
    }

    /// "VERIFIED IN CODE AND DOCS" or "ASSUMED", over its points.
    fn checked_column(title: &str, items: Vec<AnyElement>) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(6.))
            .flex_1()
            .min_w_0()
            .child(ui::overline(title))
            .children(items.into_iter().map(Self::bullet))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_answer(
        &self,
        index: usize,
        m: &Msg,
        meta: &BotMeta,
        a: &Answer,
        mine: bool,
        bot: &BotState,
        index_of: &HashMap<&str, usize>,
        team_channel: (&str, &str),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut sections: Vec<AnyElement> = Vec::new();
        if let Some(st) = bot.stale.get(&m.id) {
            sections.push(self.render_stale(index, st, &meta.range[1], index_of, team_channel, cx));
        }
        if let (Some(parts), Some(read)) = (meta.parts.filter(|p| *p > 1), meta.read) {
            sections.push(
                div()
                    .flex()
                    .gap(ui::sp(8.))
                    .py(ui::sp(12.))
                    .ty(Type::Label)
                    .font_weight(FontWeight::NORMAL)
                    .line_height(px(18.))
                    .text_color(theme::text_muted())
                    .child(
                        icon("alert-circle", 14.)
                            .mt(px(2.))
                            .text_color(theme::attention()),
                    )
                    .child({
                        let lead = format!("{read} messages is more than one read.");
                        let bold = 0..lead.len();
                        div().flex_1().min_w_0().child(
                            gpui::StyledText::new(format!(
                                "{lead} @bot summed them up in {parts} parts first, then judged \
                                 the summaries. A detail may be missing. Cite numbers still \
                                 point at the messages."
                            ))
                            .with_highlights([(
                                bold,
                                gpui::HighlightStyle {
                                    font_weight: Some(FontWeight::MEDIUM),
                                    ..Default::default()
                                },
                            )]),
                        )
                    })
                    .into_any_element(),
            );
        }
        for (k, idea) in a.ideas.iter().enumerate() {
            // The cites sit in the reason's text, so they flow with it.
            let mut text = idea.reason.clone();
            let mut chips = Vec::new();
            let mut targets = Vec::new();
            for n in &idea.cites {
                text.push(' ');
                let start = text.len();
                text.push_str(&format!("\u{a0}{n}\u{a0}"));
                chips.push(start..text.len());
                targets.push(
                    meta.cites
                        .get(&n.to_string())
                        .and_then(|id| Some((*index_of.get(id.as_str())?, id.clone()))),
                );
            }
            let chip = gpui::HighlightStyle {
                background_color: Some(theme::fill_selected()),
                color: Some(theme::text()),
                ..Default::default()
            };
            let styled = gpui::StyledText::new(text)
                .with_highlights(chips.iter().map(|r| (r.clone(), chip)).collect::<Vec<_>>());
            let view = cx.entity().downgrade();
            let reason = gpui::InteractiveText::new(
                SharedString::from(format!("teams-reason-{index}-{k}")),
                styled,
            )
            .on_click(chips, move |i, _, cx| {
                if let Some(Some((row, id))) = targets.get(i).cloned() {
                    let _ = view.update(cx, |this, cx| this.jump_to(row, id, cx));
                }
            });
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(4.))
                    .py(ui::sp(12.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(8.))
                            .child(Self::verdict_chip(idea.verdict))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .ty(Type::Body)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::text())
                                    .child(format!("{} · {}", idea.label, idea.title)),
                            ),
                    )
                    .child(
                        div()
                            .ty(Type::Body)
                            .text_color(theme::text_muted())
                            .child(reason),
                    )
                    .into_any_element(),
            );
        }
        let verified: Vec<AnyElement> = a
            .verified
            .iter()
            .enumerate()
            .map(|(k, v)| {
                let line = format!("{}: {}", v.source, v.text);
                if v.source.starts_with("https://") {
                    let url = v.source.clone();
                    div()
                        .id(SharedString::from(format!("teams-source-{index}-{k}")))
                        .map(ui::focusable)
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::link()))
                        .tooltip(ui::tip("Open the page"))
                        .child(line)
                        .on_click(move |_, _, cx| cx.open_url(&url))
                        .into_any_element()
                } else {
                    div().child(line).into_any_element()
                }
            })
            .collect();
        let assumed: Vec<AnyElement> = a
            .assumed
            .iter()
            .map(|s| div().child(s.clone()).into_any_element())
            .collect();
        let columns = !verified.is_empty() || !assumed.is_empty();
        let unchecked = (!a.unchecked.is_empty()).then(|| {
            div()
                .pt(ui::sp(if columns { 8. } else { 12. }))
                .pb(ui::sp(12.))
                .ty(Type::Label)
                .font_weight(FontWeight::NORMAL)
                .line_height(px(18.))
                .text_color(theme::text_subtle())
                .child(format!("Could not check: {}", a.unchecked.join(" ")))
        });
        if columns || unchecked.is_some() {
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .when(columns, |d| {
                        d.child(
                            div()
                                .flex()
                                .gap(ui::sp(16.))
                                .py(ui::sp(12.))
                                .child(if verified.is_empty() {
                                    div().flex_1()
                                } else {
                                    Self::checked_column("Verified in code and docs", verified)
                                })
                                .child(if assumed.is_empty() {
                                    div().flex_1()
                                } else {
                                    Self::checked_column("Assumed", assumed)
                                }),
                        )
                    })
                    .children(unchecked)
                    .into_any_element(),
            );
        }
        let decision = self.render_proposed(index, m, &a.decision, mine, bot, team_channel, cx);
        div()
            .mt(ui::sp(4.))
            .flex()
            .flex_col()
            .pt(ui::sp(4.))
            .px(ui::sp(16.))
            .pb(ui::sp(16.))
            .rounded(ui::rad(ui::RADIUS_XL))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::fill_raised())
            .children(sections.into_iter().enumerate().map(|(i, s)| {
                div()
                    .when(i > 0, |d| {
                        d.border_t_1().border_color(theme::border_subtle())
                    })
                    .child(s)
            }))
            .child(decision)
            .into_any_element()
    }

    /// "Based on an earlier version": a message in the range changed after
    /// the answer. Jump to it, or ask @bot again on the same range.
    fn render_stale(
        &self,
        index: usize,
        st: &Stale,
        ask_id: &str,
        index_of: &HashMap<&str, usize>,
        team_channel: (&str, &str),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let target = index_of.get(st.id.as_str()).copied();
        let id = st.id.clone();
        let (t, c, ask) = (
            team_channel.0.to_string(),
            team_channel.1.to_string(),
            ask_id.to_string(),
        );
        let running = self
            .hub
            .read(cx)
            .bot
            .runs
            .iter()
            .any(|r| r.ask_id == ask_id && matches!(r.state, RunState::Running(_)));
        div()
            .flex()
            .items_center()
            .gap(ui::sp(8.))
            .py(ui::sp(12.))
            .ty(Type::Label)
            .font_weight(FontWeight::NORMAL)
            .text_color(theme::text_muted())
            .child(icon("pencil-edit01", 14.).text_color(theme::attention()))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .gap(ui::sp(4.))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .child("Based on an earlier version."),
                    )
                    .child(format!("{} edited message", st.login))
                    .child(
                        ui::text(Type::Caption, theme::text(), st.n.to_string())
                            .px(ui::sp(4.))
                            .rounded(ui::rad(ui::RADIUS_SM))
                            .bg(theme::fill_selected())
                            .font_family(theme::FONT_MONO),
                    )
                    .child("after this answer."),
            )
            .children(target.map(|row| {
                ui::button(
                    ("teams-stale-see", index),
                    "See the edit",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.jump_to(row, id.clone(), cx)))
            }))
            .child(
                ui::button(
                    ("teams-stale-again", index),
                    if running { "Asking…" } else { "Ask again" },
                    ButtonKind::Outline,
                    !running,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.hub.update(cx, |h, cx| {
                        // The project the first answer read, if that run is
                        // still here; else the one the window shows now.
                        let project = h
                            .bot
                            .runs
                            .iter()
                            .find(|r| r.ask_id == ask)
                            .map_or_else(|| h.bot.project.clone(), |r| r.project.clone());
                        h.start_bot(&t, &c, &ask, project, cx)
                    })
                })),
            )
            .into_any_element()
    }

    /// The decision @bot proposes, and what this user can do with it.
    #[allow(clippy::too_many_arguments)]
    fn render_proposed(
        &self,
        index: usize,
        m: &Msg,
        decision: &str,
        mine: bool,
        bot: &BotState,
        team_channel: (&str, &str),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sent = m.delivery == Delivery::Sent;
        let rejected = bot.rejected.contains(&m.id);
        // Accepted here, but not stored yet: the team doesn't have it.
        let pending = !mine && bot.accepting.contains(&m.id);
        let (t, c, id) = (
            team_channel.0.to_string(),
            team_channel.1.to_string(),
            m.id.clone(),
        );
        let (rid, aid) = (id.clone(), id.clone());
        // Only on a stored answer: a follow-up that replies to one the team
        // doesn't have finds no range to read.
        let ask = ui::button(
            ("teams-bot-more", index),
            "Ask for more detail",
            ButtonKind::Ghost,
            sent,
        )
        .when(sent, |b| {
            b.on_click(cx.listener(move |this, _, window, cx| {
                this.reply_to = Some(aid.clone());
                let text = more_detail_text(this.composer.read(cx).text());
                this.composer.update(cx, |i, cx| i.set_text(text, cx));
                let handle = this.composer.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                cx.notify();
            }))
        });
        let actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(ui::sp(8.))
            .when(mine, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(6.))
                        .pr(ui::sp(4.))
                        .child(icon("tick02", 14.).text_color(theme::success()))
                        .child(ui::text(
                            Type::Label,
                            theme::text_muted(),
                            "You accepted this",
                        )),
                )
            })
            .when(pending, |d| {
                d.child(
                    ui::text(
                        Type::Label,
                        theme::text_subtle(),
                        "Sending your acceptance…",
                    )
                    .pr(ui::sp(4.)),
                )
            })
            .when(!mine && !pending, |d| {
                d.child(
                    ui::button(
                        ("teams-bot-accept", index),
                        "Accept decision",
                        ButtonKind::Primary,
                        sent,
                    )
                    .when(sent, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| {
                            this.hub
                                .update(cx, |h, cx| h.accept_decision(&t, &c, &id, cx));
                        }))
                    }),
                )
                .child(if rejected {
                    ui::text(
                        Type::Label,
                        theme::text_subtle(),
                        "You turned it down on this device",
                    )
                    .px(ui::sp(4.))
                    .into_any_element()
                } else {
                    ui::button(
                        ("teams-bot-reject", index),
                        "Reject",
                        ButtonKind::Outline,
                        true,
                    )
                    .tooltip(ui::tip("Only on this device: the team is not told"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.hub.update(cx, |h, cx| h.reject_decision(&rid, cx));
                    }))
                    .into_any_element()
                })
            })
            .child(ask);
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(12.))
            .p(ui::sp(12.))
            .rounded(ui::nested(ui::RADIUS_XL, 16.))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::sunk())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(4.))
                    .child(ui::overline("Proposed decision"))
                    .child(
                        div()
                            .ty(Type::Body)
                            .line_height(px(20.))
                            .text_color(theme::text())
                            .child(decision.to_string()),
                    ),
            )
            .child(actions)
            .into_any_element()
    }

    /// "Decided. Karlo and Dani accepted …", under the answer.
    pub(super) fn render_decided(
        &self,
        row: usize,
        m: &Msg,
        bot: &BotState,
        team_id: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let who: Vec<String> = bot
            .decided
            .get(&m.id)
            .map(|a| a.iter().map(|a| a.login.clone()).collect())
            .unwrap_or_default();
        let title = bot::meta(m)
            .and_then(|x| x.answer)
            .map(|a| decisions::title_of(&a.decision))
            .unwrap_or_default();
        let (t, id) = (team_id.to_string(), m.id.clone());
        let reversed = bot.reversed.get(&m.id);
        let (icon_name, color, line) = match reversed {
            Some(r) => {
                let why = if r.reason.is_empty() {
                    String::new()
                } else {
                    format!(" {}", sentence(&r.reason))
                };
                (
                    "undo",
                    theme::attention(),
                    format!("Reversed. {} reversed “{title}”.{why}", r.login),
                )
            }
            None => (
                "tick-double02",
                theme::success(),
                format!("Decided. {} accepted “{title}”.", names(&who)),
            ),
        };
        let lead = if reversed.is_some() {
            "Reversed.".len()
        } else {
            "Decided.".len()
        };
        div()
            .flex()
            .items_center()
            .gap(ui::sp(8.))
            .mx(ui::sp(16.))
            .my(ui::sp(8.))
            .pl(ui::sp(12.))
            .pr(ui::sp(4.))
            .py(ui::sp(4.))
            .rounded(ui::rad(ui::RADIUS))
            .border_1()
            .border_color(theme::border_subtle())
            .ty(Type::Label)
            .font_weight(FontWeight::NORMAL)
            .text_color(theme::text_muted())
            .child(icon(icon_name, 14.).text_color(color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(gpui::StyledText::new(line).with_highlights([(
                        0..lead,
                        gpui::HighlightStyle {
                            font_weight: Some(FontWeight::MEDIUM),
                            color: Some(theme::text()),
                            ..Default::default()
                        },
                    )])),
            )
            .child(
                ui::button(
                    ("teams-open-decision", row),
                    "Open in Decisions",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.decision = Some(id.clone());
                    this.show(Pane::Decisions(t.clone()), window, cx)
                })),
            )
            .into_any_element()
    }

    /// A run of @bot on this device, under the message that asked: what it
    /// is doing, or why it stopped.
    pub(super) fn render_run(
        &self,
        row: usize,
        run: &Run,
        bot: &BotState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (ask, again) = (run.ask_id.clone(), run.ask_id.clone());
        let card = div()
            .mt(ui::sp(4.))
            .flex()
            .items_center()
            .gap(ui::sp(12.))
            .px(ui::sp(16.))
            .py(ui::sp(12.))
            .rounded(ui::rad(ui::RADIUS_XL))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::fill_raised());
        let card = match &run.state {
            RunState::Running(step) => {
                let done = match step {
                    Step::Summarizing { part, .. } => part.saturating_sub(1),
                    Step::Reading => run.parts,
                };
                let progress = (run.parts > 0).then(|| {
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .ty(Type::Caption)
                        .font_family(theme::FONT_MONO)
                        .text_color(theme::text_faint())
                        .child(
                            div()
                                .flex()
                                .gap(ui::sp(2.))
                                .children((0..run.parts).map(|i| {
                                    div().w(px(3.)).h(px(10.)).rounded(ui::rad(1.)).bg(
                                        if i < done {
                                            theme::text_muted()
                                        } else {
                                            theme::fill_selected()
                                        },
                                    )
                                })),
                        )
                        .child(format!("{done} of {} parts", run.parts))
                });
                card.child(ui::spinner(("teams-bot-spin", row), 14., theme::accent()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .ty(Type::Body)
                            .text_color(theme::text())
                            .child(run.label()),
                    )
                    .children(progress)
                    .child(
                        ui::button(("teams-bot-stop", row), "Stop", ButtonKind::Ghost, true)
                            .tooltip(ui::tip("Stop @bot. Nothing is posted"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.hub.update(cx, |h, cx| h.stop_bot(&ask, cx));
                            })),
                    )
            }
            RunState::Failed(why) => {
                let (title, body) = failed_text(run, why);
                card.child(icon("cancel-circle", 14.).text_color(theme::error()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(ui::sp(3.))
                            .flex_1()
                            .min_w_0()
                            .child(
                                ui::text(Type::Body, theme::text(), title)
                                    .font_weight(FontWeight::MEDIUM),
                            )
                            .child(
                                ui::text(Type::Label, theme::text_subtle(), body)
                                    .font_weight(FontWeight::NORMAL),
                            ),
                    )
                    .child(
                        ui::button(
                            ("teams-bot-retry", row),
                            "Try again",
                            ButtonKind::Outline,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.hub.update(cx, |h, cx| h.retry_bot(&again, cx));
                        })),
                    )
                    .child(
                        ui::icon_button(("teams-bot-dismiss", row), "cancel01", 12., "Dismiss")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.hub.update(cx, |h, cx| h.stop_bot(&ask, cx));
                            })),
                    )
            }
        };
        let read = read_note(Some(run.read), (run.parts > 1).then_some(run.parts));
        div()
            .flex()
            .items_start()
            .gap(ui::sp(12.))
            .px(ui::sp(16.))
            .pt(ui::sp(8.))
            .pb(ui::sp(6.))
            .child(Self::bot_avatar())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(2.))
                    .flex_1()
                    .min_w_0()
                    .child(Self::bot_header(&bot.my_login, read, run.started))
                    .child(card),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_goes_stale_when_its_range_is_edited_after_it() {
        use super::super::super::bot::{Answer, Outcome};
        use super::super::super::crypto::Body;
        use super::super::super::hub::{Edited, apply_edits};
        let m = |id: &str, seq: u64, gid: u64, body: Body| Msg {
            id: id.into(),
            seq: Some(seq),
            at: seq,
            github_id: gid,
            login: format!("u{gid}"),
            body,
            delivery: Delivery::Sent,
            edited: None,
        };
        let mut all = vec![
            m("a", 1, 1, Body::text("idea", None)),
            m("q", 2, 2, Body::text("@bot decide", Some("a".into()))),
        ];
        let range = bot::select_range(&all, "q").unwrap();
        let outcome = Outcome {
            answer: Answer {
                ideas: Vec::new(),
                verified: Vec::new(),
                assumed: Vec::new(),
                unchecked: Vec::new(),
                decision: "Do it.".into(),
            },
            parts: 0,
        };
        let mut answer = m("bot", 3, 2, bot::bot_body(&range, 2, &outcome));
        answer.github_id = 2;
        all.push(answer.clone());
        assert_eq!(stale_of(&all, &answer), None);
        let mut edit = Body::text("a better idea", Some("a".into()));
        edit.kind = "edit".into();
        all.push(m("e", 4, 1, edit));
        apply_edits(&mut all);
        assert!(matches!(all[0].edited, Some(Edited { seq: Some(4), .. })));
        assert_eq!(
            stale_of(&all, &answer),
            Some(Stale {
                n: 1,
                login: "u1".into(),
                id: "a".into()
            })
        );
    }

    #[test]
    fn a_failed_run_says_how_far_it_got_and_when_the_limit_resets() {
        let now = 1_800_000_000;
        let why = "Claude Code stopped: Claude AI usage limit reached|1800002400";
        let (title, body) = failed_text_at((9, 23), why, "karlo-desktop", now, 0);
        assert_eq!(title, "@bot stopped after 9 of 23 parts");
        assert_eq!(
            body,
            "Claude Code on karlo-desktop hit its 5 h limit. It resets in 40 minutes. Nothing was decided."
        );
        let (title, body) = failed_text_at((3, 3), "the answer is too long to post", "x", now, 0);
        assert_eq!(title, "@bot stopped");
        assert_eq!(body, "The answer is too long to post. Nothing was decided.");
        let (title, _) = failed_text_at((0, 0), why, "x", now, 0);
        assert_eq!(title, "@bot stopped");
    }

    #[test]
    fn names_read_as_a_list() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(names(&l(&[])), "");
        assert_eq!(names(&l(&["Karlo"])), "Karlo");
        assert_eq!(names(&l(&["Karlo", "Dani"])), "Karlo and Dani");
        assert_eq!(names(&l(&["Karlo", "Dani", "Rik"])), "Karlo, Dani and Rik");
    }

    #[test]
    fn asking_for_more_detail_keeps_a_draft() {
        assert_eq!(more_detail_text(""), "@bot more detail on ");
        assert_eq!(more_detail_text("  \n"), "@bot more detail on ");
        assert_eq!(
            more_detail_text("half a long message"),
            "@bot half a long message"
        );
        assert_eq!(more_detail_text("@bot why B?"), "@bot why B?");
        assert_eq!(more_detail_text("and why B, @bot?"), "and why B, @bot?");
    }

    #[test]
    fn reasons_read_as_sentences() {
        assert_eq!(
            sentence("the channel is not open"),
            "The channel is not open."
        );
        assert_eq!(
            sentence("Claude Code stopped: limit."),
            "Claude Code stopped: limit."
        );
        assert_eq!(sentence(""), "");
    }

    #[test]
    fn the_read_note_counts_messages_and_parts() {
        assert_eq!(
            read_note(Some(23), None).as_deref(),
            Some("read 23 messages")
        );
        assert_eq!(read_note(Some(1), None).as_deref(), Some("read 1 message"));
        assert_eq!(
            read_note(Some(214), Some(4)).as_deref(),
            Some("read 214 messages, in 4 parts")
        );
        assert_eq!(read_note(Some(0), None), None);
    }

    #[test]
    fn numbers_come_from_each_runs_own_range() {
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (a, b, single) = (
            ids(&["m1", "m2", "m3"]),
            ids(&["m2", "m3", "m4"]),
            ids(&["m9"]),
        );
        let n = run_numbers([a.as_slice(), b.as_slice(), single.as_slice()]);
        assert_eq!(n.get("m1"), Some(&1));
        // m2 and m3 are [2] and [3] in one run, [1] and [2] in the other.
        assert_eq!(n.get("m2"), None);
        assert_eq!(n.get("m3"), None);
        assert_eq!(n.get("m4"), Some(&3));
        // A run of one message numbers nothing.
        assert_eq!(n.get("m9"), None);
        let same = run_numbers([a.as_slice(), a.as_slice()]);
        assert_eq!(same.get("m2"), Some(&2));
    }
}
