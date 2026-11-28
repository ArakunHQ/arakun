//! The agents a Marketplace item is for. The page keeps one layout whichever
//! agent Show for picks: an item stays in its list and says, per agent,
//! whether that agent has it on, has it off, lacks it or can't use it.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Div, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use crate::logos;
use crate::theme;
use crate::ui::Typography as _;
use crate::ui::{self, Tone, Type};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Agent {
    Claude,
    Droid,
}

impl Agent {
    /// In the order Show for and the Agents card list them. A third agent is
    /// one more entry here.
    pub(super) const ALL: [Agent; 2] = [Agent::Claude, Agent::Droid];

    pub(super) fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Droid => "Droid",
        }
    }

    pub(super) fn key(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Droid => "droid",
        }
    }

    pub(super) fn mark(self) -> logos::Mark {
        match self {
            Agent::Claude => logos::mark("claude", ""),
            Agent::Droid => logos::mark("droid", ""),
        }
    }

    pub(super) fn index(self) -> usize {
        Self::ALL.iter().position(|a| *a == self).unwrap_or(0)
    }
}

/// What the page shows the cells for: every agent, or one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Show {
    All,
    Only(Agent),
}

impl Show {
    pub(super) fn label(self) -> &'static str {
        match self {
            Show::All => "All agents",
            Show::Only(a) => a.name(),
        }
    }

    pub(super) fn every() -> impl Iterator<Item = Show> {
        std::iter::once(Show::All).chain(Agent::ALL.into_iter().map(Show::Only))
    }
}

/// How one agent stands with an item.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Use {
    On,
    /// Installed, and turned off for this agent only.
    Off,
    /// The agent could use it, and it isn't set up for it.
    Missing,
    /// The agent can't use it, and why.
    Cannot(&'static str),
}

impl Use {
    pub(super) fn has(&self) -> bool {
        matches!(self, Use::On | Use::Off)
    }

    pub(super) fn on(on: bool) -> Use {
        if on { Use::On } else { Use::Off }
    }
}

/// One `Use` per agent, in `Agent::ALL` order.
pub(super) type Uses = [Use; 2];

/// The words beside the marks in the Agents column. None when there is
/// nothing to add to the marks: every agent has it on.
pub(super) fn words(show: Show, uses: &Uses) -> Option<String> {
    match show {
        Show::Only(a) => Some(match &uses[a.index()] {
            Use::On => "On".to_string(),
            Use::Off => format!("Off for {}", a.name()),
            Use::Missing => format!("Not in {}", a.name()),
            Use::Cannot(why) => why.to_string(),
        }),
        Show::All => {
            let named = |pick: fn(&Use) -> bool| -> Vec<&'static str> {
                Agent::ALL
                    .iter()
                    .filter(|a| pick(&uses[a.index()]))
                    .map(|a| a.name())
                    .collect()
            };
            let has = named(Use::has);
            let off = named(|u| *u == Use::Off);
            let cannot = named(|u| matches!(u, Use::Cannot(_)));
            if has.is_empty() {
                return Some("Not installed".into());
            }
            if has.len() == 1 && cannot.len() + 1 == Agent::ALL.len() {
                return Some(if off.is_empty() {
                    format!("{} only", has[0])
                } else {
                    format!("{} only · off", has[0])
                });
            }
            if !off.is_empty() && off.len() == has.len() {
                return Some("Off".into());
            }
            if !off.is_empty() {
                return Some(format!("Off for {}", off.join(", ")));
            }
            let missing = named(|u| *u == Use::Missing);
            (!missing.is_empty()).then(|| format!("Not in {}", missing.join(", ")))
        }
    }
}

/// The Status cell when one agent is picked and it doesn't have the item on:
/// the words, and whether the dot is hollow. None keeps the item's own state.
pub(super) fn state_for(show: Show, uses: &Uses) -> Option<String> {
    let Show::Only(a) = show else { return None };
    match &uses[a.index()] {
        Use::On => None,
        Use::Off => Some(format!("Off for {}", a.name())),
        Use::Missing => Some(format!("Not in {}", a.name())),
        Use::Cannot(_) => Some(format!("Not for {}", a.name())),
    }
}

/// Joins two agents' lists into one row per item, matched by `key`: the first
/// list's order, then what only the second has.
pub(super) fn merge<'a, A, B>(
    a: &'a [A],
    key_a: impl Fn(&A) -> String,
    b: &'a [B],
    key_b: impl Fn(&B) -> String,
) -> Vec<(Option<&'a A>, Option<&'a B>)> {
    let mut out: Vec<(Option<&A>, Option<&B>)> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for x in a {
        let k = key_a(x).to_lowercase();
        if keys.contains(&k) {
            continue;
        }
        keys.push(k);
        out.push((Some(x), None));
    }
    for y in b {
        let k = key_b(y).to_lowercase();
        match keys.iter().position(|have| *have == k) {
            Some(i) if out[i].1.is_none() => out[i].1 = Some(y),
            Some(_) => {}
            None => {
                keys.push(k);
                out.push((None, Some(y)));
            }
        }
    }
    out
}

/// A state in an outlined pill: a dot in the state's color and the word. Hollow,
/// the item is off or missing for the picked agent.
pub(super) fn pill(word: impl Into<SharedString>, tone: Tone, hollow: bool) -> Div {
    let dot = div().flex_none().size(px(6.)).rounded_full();
    let dot = if hollow {
        dot.border_1().border_color(theme::text_subtle())
    } else {
        dot.bg(tone.color())
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(ui::sp(6.))
        .h(px(24.))
        .max_w(px(super::STATUS_W))
        .px(ui::sp(8.))
        .rounded(ui::rad(ui::RADIUS))
        .border_1()
        .border_color(theme::border())
        .bg(theme::fill_raised())
        .ty(Type::Label)
        .text_color(if hollow {
            theme::text_muted()
        } else {
            theme::text()
        })
        .child(dot)
        .child(div().truncate().child(word.into()))
}

/// The Agents column: the marks of the agents that have the item (faint where
/// it is off) and the words that say what the marks don't.
pub(super) fn cell(show: Show, uses: &Uses) -> Div {
    let mut marks = div().flex().flex_none().items_center().gap(ui::sp(2.));
    for a in Agent::ALL {
        let u = &uses[a.index()];
        if u.has() {
            marks =
                marks.child(logos::tile(a.mark(), 18.).when(*u == Use::Off, |d| d.opacity(0.4)));
        }
    }
    div()
        .flex()
        .items_center()
        .min_w_0()
        .gap(ui::sp(6.))
        .child(marks)
        .when_some(words(show, uses), |d, w| {
            d.child(
                div()
                    .min_w_0()
                    .truncate()
                    .ty(Type::Caption)
                    .text_color(theme::text_subtle())
                    .child(w),
            )
        })
}

/// Turns an item on (true) or off for one agent.
pub(super) type Toggle = Box<dyn Fn(bool, &mut gpui::Window, &mut gpui::App)>;

/// One agent's line in the Agents card: what it reads and its switch.
pub(super) struct Line {
    pub agent: Agent,
    pub uses: Use,
    /// The file the agent reads the item from, or where it would.
    pub file: String,
    /// Turns the item on or off for this agent only; none when Arakun can't.
    pub toggle: Option<Toggle>,
    pub busy: bool,
}

/// The detail pane's Agents card: one row per agent with its mark, its name,
/// the file it reads and a switch. Off for one agent, the item stays installed.
pub(super) fn card(id: &str, lines: Vec<Line>) -> Div {
    let rows: Vec<AnyElement> = lines
        .into_iter()
        .map(|line| {
            let Line {
                agent,
                uses,
                file,
                toggle,
                busy,
            } = line;
            let (detail, state): (String, AnyElement) = match &uses {
                Use::Cannot(why) => (
                    why.to_string(),
                    pill(format!("Not for {}", agent.name()), Tone::Neutral, true)
                        .into_any_element(),
                ),
                Use::Missing => (
                    file,
                    pill("Not set up", Tone::Neutral, true).into_any_element(),
                ),
                Use::On | Use::Off => {
                    let on = uses == Use::On;
                    let state = if busy {
                        ui::spinner(14., theme::text_faint()).into_any_element()
                    } else if let Some(toggle) = toggle {
                        div()
                            .id(SharedString::from(format!("{id}-agent-{}", agent.key())))
                            .map(|d| ui::as_switch(d, agent.name(), on))
                            .cursor_pointer()
                            .child(ui::switch(on))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                toggle(!on, window, cx)
                            })
                            .into_any_element()
                    } else if on {
                        pill("On", Tone::Success, false).into_any_element()
                    } else {
                        pill("Off for this agent", Tone::Neutral, true).into_any_element()
                    };
                    (file, state)
                }
            };
            div()
                .flex()
                .items_center()
                .gap(ui::sp(10.))
                .px(ui::sp(12.))
                .py(ui::sp(8.))
                .border_t_1()
                .border_color(theme::border_subtle())
                .child(logos::tile(agent.mark(), 24.).when(!uses.has(), |d| d.opacity(0.4)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .ty(Type::Body)
                                .text_color(theme::text())
                                .child(agent.name()),
                        )
                        .child(
                            div()
                                .truncate()
                                .ty(if uses.has() || uses == Use::Missing {
                                    Type::Mono
                                } else {
                                    Type::Caption
                                })
                                .text_color(theme::text_subtle())
                                .child(detail),
                        ),
                )
                .child(state)
                .into_any_element()
        })
        .collect();
    div()
        .flex()
        .flex_col()
        .gap(ui::sp(8.))
        .child(ui::overline("Agents"))
        .child(
            div()
                .flex()
                .flex_col()
                .rounded(ui::rad(ui::RADIUS_LG))
                .border_1()
                .border_color(theme::border())
                .bg(theme::background())
                // The first row's top border would double the card's.
                .overflow_hidden()
                .children(rows),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_agents_says_what_the_marks_do_not() {
        let all = Show::All;
        assert_eq!(words(all, &[Use::On, Use::On]), None);
        assert_eq!(
            words(all, &[Use::On, Use::Off]),
            Some("Off for Droid".into())
        );
        assert_eq!(
            words(all, &[Use::On, Use::Cannot("no")]),
            Some("Claude Code only".into())
        );
        assert_eq!(
            words(all, &[Use::On, Use::Missing]),
            Some("Not in Droid".into())
        );
        assert_eq!(words(all, &[Use::Off, Use::Off]), Some("Off".into()));
        assert_eq!(
            words(all, &[Use::Missing, Use::Missing]),
            Some("Not installed".into())
        );
    }

    #[test]
    fn one_agent_shows_its_own_state() {
        let droid = Show::Only(Agent::Droid);
        assert_eq!(words(droid, &[Use::On, Use::On]), Some("On".into()));
        assert_eq!(
            words(droid, &[Use::On, Use::Cannot("A claude.ai connector")]),
            Some("A claude.ai connector".into())
        );
        assert_eq!(state_for(droid, &[Use::On, Use::On]), None);
        assert_eq!(
            state_for(droid, &[Use::On, Use::Off]),
            Some("Off for Droid".into())
        );
        assert_eq!(
            state_for(droid, &[Use::On, Use::Cannot("x")]),
            Some("Not for Droid".into())
        );
        assert_eq!(
            state_for(Show::Only(Agent::Claude), &[Use::Missing, Use::On]),
            Some("Not in Claude Code".into())
        );
        assert_eq!(state_for(Show::All, &[Use::Missing, Use::On]), None);
    }

    #[test]
    fn merges_one_row_per_item() {
        let a = ["posthog", "linear", "Linear"];
        let b = ["Linear", "figma"];
        let rows = merge(&a, |s| s.to_string(), &b, |s| s.to_string());
        let got: Vec<(Option<&str>, Option<&str>)> =
            rows.iter().map(|(x, y)| (x.copied(), y.copied())).collect();
        assert_eq!(
            got,
            vec![
                (Some("posthog"), None),
                (Some("linear"), Some("Linear")),
                (None, Some("figma")),
            ]
        );
    }

    #[test]
    fn show_for_lists_every_agent() {
        let labels: Vec<&str> = Show::every().map(Show::label).collect();
        assert_eq!(labels, ["All agents", "Claude Code", "Droid"]);
    }
}
