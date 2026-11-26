//! What a reply or a stop from Linear leads to, for a session it started.
//! The relay only says that a reply exists: its text is read from Linear
//! with the person's own key, never taken from the relay.

use std::collections::HashMap;

use super::runs::{Run, RunState};
use crate::agent::Thread;
use crate::phone::linear::{Delivery, DeliveryKind};

/// Whether the first look at a delivery of `kind` refuses it for lack of a
/// Linear sign-in here. A stop needs no key. A reply needs one too, but the
/// switch is asked first: [`reply_source`] does both.
pub fn needs_sign_in(kind: DeliveryKind) -> bool {
    match kind {
        DeliveryKind::Created => true,
        DeliveryKind::Prompt | DeliveryKind::Stop => false,
    }
}

/// Where a reply's text comes from, or why it is not passed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source<'a> {
    /// The switch is off: nothing from Linear reaches a session.
    Off,
    /// No Linear sign-in for the reply's workspace, so no key to read it with.
    NoSignIn,
    /// The relay named no activity to read.
    Unreadable,
    /// Read this agent activity from Linear with the person's own key.
    Read(&'a str),
}

/// The checks a reply passes before its text is read. The relay's own copy
/// of the text (`body`) is never used: whoever holds the relay could
/// otherwise type into a session that commits and pushes.
pub fn reply_source(d: &Delivery, switch_on: bool, signed_in: bool) -> Source<'_> {
    if !switch_on {
        Source::Off
    } else if !signed_in {
        Source::NoSignIn
    } else {
        match d.activity.as_deref() {
            Some(activity) if !activity.is_empty() => Source::Read(activity),
            _ => Source::Unreadable,
        }
    }
}

/// The state of the run a follow-up names. A run of another organization
/// counts as none: the agent session id alone is not trusted.
pub fn run_state(run: Option<&Run>, organization: &str) -> Option<RunState> {
    run.filter(|r| r.organization == organization)
        .map(|r| r.state)
}

/// Whether a stop must wait: other work for its agent session is in flight
/// (a new delegation being read, or a reply whose text is being read).
/// Run now, it would find no run yet, or be undone by the reply.
pub fn hold_stop(in_flight: &HashMap<String, String>, agent_session: &str) -> bool {
    in_flight.values().any(|a| a == agent_session)
}

/// The held stops for `agent_session` once nothing else for it is in flight,
/// taken out of `held` and `in_flight`.
pub fn release_stops(
    held: &mut Vec<Delivery>,
    in_flight: &mut HashMap<String, String>,
    agent_session: &str,
) -> Vec<Delivery> {
    let busy = in_flight
        .iter()
        .any(|(id, a)| a == agent_session && !held.iter().any(|h| &h.id == id));
    if busy {
        return Vec::new();
    }
    let (out, keep): (Vec<Delivery>, Vec<Delivery>) = std::mem::take(held)
        .into_iter()
        .partition(|h| h.agent_session == agent_session);
    *held = keep;
    for h in &out {
        in_flight.remove(&h.id);
    }
    out
}

/// What a reply from Linear leads to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowUp {
    /// No run, or its session is gone here.
    NoRun,
    /// The run waits for a slot: there is no session to send it to yet.
    NotStarted,
    /// The run ended and another run works on its issue now: a reply here
    /// would put two sessions in one worktree.
    Busy,
    Unreadable,
    Send,
}

/// A run that started takes a reply, even one that ended: the person may
/// want more done. `text` is what was read from Linear, if anything was.
/// `issue_busy`: a run for the same issue is waiting, running or asking.
pub fn follow_up(
    run: Option<RunState>,
    session_exists: bool,
    issue_busy: bool,
    text: Option<&str>,
) -> FollowUp {
    match run {
        None => FollowUp::NoRun,
        Some(RunState::Waiting) => FollowUp::NotStarted,
        Some(_) if !session_exists => FollowUp::NoRun,
        // Busy with an ended run means another run: this one is not active.
        Some(RunState::Done | RunState::Interrupted) if issue_busy => FollowUp::Busy,
        Some(_) => match text {
            Some(t) if !t.trim().is_empty() => FollowUp::Send,
            _ => FollowUp::Unreadable,
        },
    }
}

/// The state a run takes once a reply from Linear is passed on to its
/// session. An asked run stays asked: the message waits in the session's
/// queue behind the question or permission. A running one is unchanged.
pub fn after_reply(state: RunState) -> Option<RunState> {
    match state {
        RunState::Done | RunState::Interrupted => Some(RunState::Running),
        RunState::Waiting | RunState::Running | RunState::Asked => None,
    }
}

/// What waits on a person in the session a reply is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    Nothing,
    /// A question the agent asked, with no secret field.
    Question,
    /// A permission prompt, alone or with anything else.
    Permission,
    /// A secret the agent asked for, as a card or as a question's field.
    Secret,
}

/// What waits, from what the session holds. A permission comes first, as on
/// the desktop: with one pending nothing else is touched. `question` says
/// whether the pending question has a secret field.
pub fn pending(permission: bool, secret_card: bool, question: Option<bool>) -> Pending {
    if permission {
        Pending::Permission
    } else if secret_card || question == Some(true) {
        Pending::Secret
    } else if question.is_some() {
        Pending::Question
    } else {
        Pending::Nothing
    }
}

pub fn pending_in(t: &Thread) -> Pending {
    let question = t
        .pending_question()
        .map(|q| q.questions.iter().any(|x| x.secret));
    pending(
        t.pending_permission().is_some(),
        t.pending_secret().is_some(),
        question,
    )
}

/// Whether a reply from Linear first skips what the session waits for. Only
/// a plain question: the person answered it in their own words, in Linear.
/// A permission is never answered from Linear, and a secret never leaves
/// Arakun, so neither is answered and the reply goes on as any message.
pub fn skips_question(pending: Pending) -> bool {
    pending == Pending::Question
}

/// The most questions one reply skips, should an agent have asked several.
pub const SKIPS: usize = 8;

/// What a stop from Linear leads to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// No run: nothing to stop and nobody to tell.
    Nothing,
    /// Already over: Linear hears it stopped.
    Report,
    /// Waiting for a slot: it leaves the queue.
    Dequeue,
    /// At work: its turn is cancelled.
    Cancel,
}

pub fn stop(run: Option<RunState>) -> Stop {
    match run {
        None => Stop::Nothing,
        Some(RunState::Waiting) => Stop::Dequeue,
        Some(RunState::Running | RunState::Asked) => Stop::Cancel,
        Some(RunState::Done | RunState::Interrupted) => Stop::Report,
    }
}

pub const STOPPED: &str = "Stopped.";

pub fn no_session_line(computer: &str) -> String {
    format!("{computer} has no session for this any more. Delegate the issue again.")
}

pub fn unread_line(computer: &str) -> String {
    format!(
        "Arakun on {computer} could not read that message from Linear. Send it again, or type \
         it in the session in Arakun."
    )
}

/// A reply while the switch is off. Delegating again is the wrong advice for
/// a reply, so it has its own line.
pub fn reply_off_line(computer: &str) -> String {
    format!(
        "{computer} no longer takes work from Linear, so your message was not passed on. Turn \
         it on in Arakun's Linear tab, then send it again."
    )
}

pub fn reply_no_sign_in_line(computer: &str) -> String {
    format!(
        "{computer} is not signed in to this Linear workspace, so it could not read your \
         message. Sign in in Arakun's Linear tab, then send it again."
    )
}

/// What Linear hears once a reply reached its session. Linear shows the
/// agent as starting after a person's message until the agent says
/// something, so this goes at once. `pending` is what still waits on the
/// person after a question was skipped; `busy` means the turn before it has
/// not ended. Never the reply's text.
pub fn passed_line(computer: &str, pending: Pending, busy: bool) -> String {
    let passed = format!("Passed your message to the session on {computer}.");
    match pending {
        Pending::Permission => format!(
            "{passed} It reads it once you answer its permission request in Arakun or on your \
             phone."
        ),
        Pending::Secret => {
            format!("{passed} It reads it once you give it what it asked for in Arakun.")
        }
        Pending::Question | Pending::Nothing if busy => {
            format!("{passed} It reads it when its current turn ends.")
        }
        Pending::Question | Pending::Nothing => passed,
    }
}

pub fn not_started_line(computer: &str) -> String {
    format!(
        "This has not started yet on {computer}. Your message will not be passed on; send it \
         again once it has started."
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const ALL: [RunState; 5] = [
        RunState::Waiting,
        RunState::Running,
        RunState::Asked,
        RunState::Done,
        RunState::Interrupted,
    ];

    #[test]
    fn a_reply_without_a_run_has_no_session() {
        for exists in [false, true] {
            for busy in [false, true] {
                for text in [None, Some(""), Some("go on")] {
                    assert_eq!(follow_up(None, exists, busy, text), FollowUp::NoRun);
                }
            }
        }
    }

    #[test]
    fn a_reply_to_a_waiting_run_is_not_passed_on() {
        for exists in [false, true] {
            for text in [None, Some("go on")] {
                // A waiting run makes its own issue busy.
                assert_eq!(
                    follow_up(Some(RunState::Waiting), exists, true, text),
                    FollowUp::NotStarted
                );
            }
        }
    }

    #[test]
    fn a_reply_whose_session_was_deleted_has_no_session() {
        for state in ALL.into_iter().filter(|s| *s != RunState::Waiting) {
            for text in [None, Some("go on")] {
                assert_eq!(follow_up(Some(state), false, false, text), FollowUp::NoRun);
                assert_eq!(follow_up(Some(state), false, true, text), FollowUp::NoRun);
            }
        }
    }

    #[test]
    fn a_reply_without_readable_text_is_unreadable() {
        for state in ALL.into_iter().filter(|s| *s != RunState::Waiting) {
            for text in [None, Some(""), Some("  \n\t ")] {
                assert_eq!(
                    follow_up(Some(state), true, false, text),
                    FollowUp::Unreadable,
                    "{state:?} {text:?}"
                );
            }
        }
    }

    #[test]
    fn a_reply_reaches_any_session_that_started() {
        for state in [
            RunState::Running,
            RunState::Asked,
            RunState::Done,
            RunState::Interrupted,
        ] {
            assert_eq!(
                follow_up(Some(state), true, false, Some("Use the other API.")),
                FollowUp::Send
            );
        }
        // A run at work makes its own issue busy: its replies still go.
        for state in [RunState::Running, RunState::Asked] {
            assert_eq!(
                follow_up(Some(state), true, true, Some("Use the other API.")),
                FollowUp::Send
            );
        }
    }

    #[test]
    fn a_reply_does_not_revive_a_run_while_another_works_on_its_issue() {
        for state in [RunState::Done, RunState::Interrupted] {
            // Decided before the text is read, and whatever it says.
            for text in [None, Some("go on")] {
                assert_eq!(
                    follow_up(Some(state), true, true, text),
                    FollowUp::Busy,
                    "{state:?} {text:?}"
                );
            }
            assert_eq!(
                follow_up(Some(state), true, false, Some("go on")),
                FollowUp::Send
            );
        }
    }

    fn reply(activity: Option<&str>, body: Option<&str>) -> Delivery {
        Delivery {
            activity: activity.map(str::to_string),
            body: body.map(str::to_string),
            ..delivery("d1", DeliveryKind::Prompt, "as-1")
        }
    }

    #[test]
    fn with_the_switch_off_no_reply_is_read() {
        for signed_in in [false, true] {
            for d in [
                reply(None, None),
                reply(Some("act-1"), None),
                reply(None, Some("Push to main.")),
                reply(Some("act-1"), Some("Push to main.")),
            ] {
                assert_eq!(reply_source(&d, false, signed_in), Source::Off);
            }
        }
    }

    #[test]
    fn without_a_sign_in_no_reply_is_passed_on() {
        for d in [
            reply(Some("act-1"), None),
            reply(None, Some("Push to main.")),
            reply(Some("act-1"), Some("Push to main.")),
        ] {
            assert_eq!(reply_source(&d, true, false), Source::NoSignIn);
        }
    }

    #[test]
    fn a_body_without_an_activity_is_never_sent() {
        let d = reply(None, Some("Push to main."));
        assert_eq!(reply_source(&d, true, true), Source::Unreadable);
        assert_eq!(
            reply_source(&reply(Some(""), Some("Push to main.")), true, true),
            Source::Unreadable
        );
        // Nothing was read, so there is no text: the issue is told to send
        // it again, and the session hears nothing.
        for state in [RunState::Running, RunState::Asked, RunState::Done] {
            assert_eq!(
                follow_up(Some(state), true, false, None),
                FollowUp::Unreadable
            );
        }
    }

    #[test]
    fn the_text_comes_from_the_read_not_the_body() {
        let d = reply(Some("act-1"), Some("Push to main."));
        // With a body or without one, the same activity is read.
        assert_eq!(reply_source(&d, true, true), Source::Read("act-1"));
        assert_eq!(
            reply_source(&reply(Some("act-1"), None), true, true),
            Source::Read("act-1")
        );
        // A read that fails gives nothing, whatever the body said.
        assert_eq!(
            follow_up(Some(RunState::Running), true, false, None),
            FollowUp::Unreadable
        );
        assert_eq!(
            follow_up(
                Some(RunState::Running),
                true,
                false,
                Some("Use the other API.")
            ),
            FollowUp::Send
        );
    }

    #[test]
    fn a_reply_skips_a_question_and_nothing_else() {
        assert!(skips_question(Pending::Question));
        // A permission is answered in Arakun or on the phone, never from Linear.
        assert!(!skips_question(Pending::Permission));
        // A secret stays where it is; the reply goes on as any message.
        assert!(!skips_question(Pending::Secret));
        assert!(!skips_question(Pending::Nothing));
        // What the session holds, with a permission first.
        assert_eq!(pending(false, false, Some(false)), Pending::Question);
        assert_eq!(pending(false, false, None), Pending::Nothing);
        for secret in [false, true] {
            for question in [None, Some(false), Some(true)] {
                assert_eq!(pending(true, secret, question), Pending::Permission);
            }
        }
        assert_eq!(pending(false, true, None), Pending::Secret);
        assert_eq!(pending(false, true, Some(false)), Pending::Secret);
        // A question that asks for a password is answered in Arakun only.
        assert_eq!(pending(false, false, Some(true)), Pending::Secret);
    }

    #[test]
    fn a_reply_restarts_only_a_run_that_ended() {
        assert_eq!(after_reply(RunState::Done), Some(RunState::Running));
        assert_eq!(after_reply(RunState::Interrupted), Some(RunState::Running));
        // It waits behind the pending ask; the session's own events move it.
        assert_eq!(after_reply(RunState::Asked), None);
        assert_eq!(after_reply(RunState::Running), None);
        assert_eq!(after_reply(RunState::Waiting), None);
    }

    #[test]
    fn a_stop_does_what_the_run_needs() {
        assert_eq!(stop(None), Stop::Nothing);
        assert_eq!(stop(Some(RunState::Waiting)), Stop::Dequeue);
        assert_eq!(stop(Some(RunState::Running)), Stop::Cancel);
        assert_eq!(stop(Some(RunState::Asked)), Stop::Cancel);
        assert_eq!(stop(Some(RunState::Done)), Stop::Report);
        assert_eq!(stop(Some(RunState::Interrupted)), Stop::Report);
    }

    fn delivery(id: &str, kind: DeliveryKind, agent_session: &str) -> Delivery {
        Delivery {
            id: id.into(),
            kind,
            agent_session: agent_session.into(),
            issue: "i1".into(),
            organization: "w1".into(),
            activity: None,
            body: None,
        }
    }

    fn flights(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(id, a)| (id.to_string(), a.to_string()))
            .collect()
    }

    #[test]
    fn a_stop_needs_no_sign_in_and_a_reply_asks_the_switch_first() {
        use super::super::start::{First, first_step};
        assert!(needs_sign_in(DeliveryKind::Created));
        assert!(!needs_sign_in(DeliveryKind::Prompt));
        assert!(!needs_sign_in(DeliveryKind::Stop));
        // Not signed in to the workspace: what the first look decides.
        let first = |kind| first_step(false, false, true, !needs_sign_in(kind));
        assert_eq!(first(DeliveryKind::Stop), First::Read);
        assert_eq!(first(DeliveryKind::Created), First::RefuseNoSignIn);
        // A reply goes on to its own checks, which still refuse it.
        assert_eq!(first(DeliveryKind::Prompt), First::Read);
        let d = reply(Some("act-1"), Some("Push to main."));
        assert_eq!(reply_source(&d, true, false), Source::NoSignIn);
        assert_eq!(reply_source(&d, false, false), Source::Off);
    }

    #[test]
    fn a_run_of_another_organization_counts_as_none() {
        let run = Run {
            agent_session: "as-1".into(),
            organization: "w1".into(),
            issue: "i1".into(),
            identifier: "ENG-1".into(),
            project: PathBuf::from("/p"),
            environment: None,
            branch: String::new(),
            session: None,
            state: RunState::Running,
            reported_at: 0,
            asked: None,
            stopped: false,
        };
        assert_eq!(run_state(Some(&run), "w1"), Some(RunState::Running));
        assert_eq!(run_state(Some(&run), "w2"), None);
        assert_eq!(run_state(None, "w1"), None);
        assert_eq!(stop(run_state(Some(&run), "w2")), Stop::Nothing);
        assert_eq!(
            follow_up(run_state(Some(&run), "w2"), true, false, Some("go on")),
            FollowUp::NoRun
        );
    }

    #[test]
    fn a_stop_waits_for_work_in_flight_on_its_agent_session() {
        let in_flight = flights(&[("d1", "as-1")]);
        assert!(hold_stop(&in_flight, "as-1"));
        assert!(!hold_stop(&in_flight, "as-2"));
        assert!(!hold_stop(&HashMap::new(), "as-1"));
    }

    #[test]
    fn held_stops_go_once_the_work_has_landed() {
        // A delegation (d1) is read; two stops (s1, s2) for it wait, and one
        // for another agent session (s3) too.
        let mut in_flight = flights(&[
            ("d1", "as-1"),
            ("s1", "as-1"),
            ("s2", "as-1"),
            ("d2", "as-2"),
            ("s3", "as-2"),
        ]);
        let mut held = vec![
            delivery("s1", DeliveryKind::Stop, "as-1"),
            delivery("s3", DeliveryKind::Stop, "as-2"),
            delivery("s2", DeliveryKind::Stop, "as-1"),
        ];
        // d1 still in flight: nothing goes.
        assert!(release_stops(&mut held, &mut in_flight, "as-1").is_empty());
        assert_eq!(held.len(), 3);
        in_flight.remove("d1");
        let out = release_stops(&mut held, &mut in_flight, "as-1");
        let ids: Vec<&str> = out.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["s1", "s2"], "in the order they came");
        assert_eq!(
            held.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
            ["s3"]
        );
        assert_eq!(in_flight, flights(&[("d2", "as-2"), ("s3", "as-2")]));
        // Released ones are no longer in flight, so they now run at once.
        assert!(!hold_stop(&in_flight, "as-1"));
        assert!(release_stops(&mut held, &mut in_flight, "as-1").is_empty());
    }

    #[test]
    fn a_passed_reply_says_what_it_waits_behind() {
        let line = |p, busy| passed_line("rik-mac", p, busy);
        let passed = "Passed your message to the session on rik-mac.";
        assert_eq!(line(Pending::Nothing, false), passed);
        assert_eq!(
            line(Pending::Nothing, true),
            format!("{passed} It reads it when its current turn ends.")
        );
        assert_eq!(
            line(Pending::Permission, true),
            format!(
                "{passed} It reads it once you answer its permission request in Arakun or on \
                 your phone."
            )
        );
        assert_eq!(
            line(Pending::Secret, true),
            format!("{passed} It reads it once you give it what it asked for in Arakun.")
        );
    }

    #[test]
    fn the_lines_read_as_in_the_plan() {
        assert_eq!(STOPPED, "Stopped.");
        assert_eq!(
            no_session_line("rik-mac"),
            "rik-mac has no session for this any more. Delegate the issue again."
        );
        assert_eq!(
            unread_line("rik-mac"),
            "Arakun on rik-mac could not read that message from Linear. Send it again, or \
             type it in the session in Arakun."
        );
        assert_eq!(
            reply_off_line("rik-mac"),
            "rik-mac no longer takes work from Linear, so your message was not passed on. Turn \
             it on in Arakun's Linear tab, then send it again."
        );
        assert_eq!(
            reply_no_sign_in_line("rik-mac"),
            "rik-mac is not signed in to this Linear workspace, so it could not read your \
             message. Sign in in Arakun's Linear tab, then send it again."
        );
        assert_eq!(
            not_started_line("rik-mac"),
            "This has not started yet on rik-mac. Your message will not be passed on; send it \
             again once it has started."
        );
    }
}
