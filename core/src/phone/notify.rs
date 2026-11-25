//! What a notice says: short plain text built from the session, so a phone
//! shows the answer or the question and not a fixed line. And who gets it
//! when: `Notifier` holds a notice until the person is away from the computer.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use crate::agent::{AskUserRequest, Entry, PermissionRequest, ToolCall};
use crate::phone::{Device, Door, Notices, push};

/// How often a sender checks the notices waiting in its `Notifier`.
pub const CHECK_EVERY: Duration = Duration::from_secs(15);

/// What the notice is about. The apps read it as `kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Done,
    Input,
    Error,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Done => "done",
            Kind::Input => "input",
            Kind::Error => "error",
        }
    }
}

/// A notice a session would send, before it is known who gets it and when.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub session: String,
    pub kind: Kind,
    /// How long the turn took; `None` counts as long.
    pub turn_secs: Option<u64>,
    /// The finished notice, from `push::notice`.
    pub payload: String,
}

/// Whether the person is at the computer.
#[derive(Clone, Copy, Debug, Default)]
pub struct Presence {
    /// How long since they last used it, when the host can tell.
    pub idle: Option<Duration>,
    /// Whether the Arakun window has focus.
    pub focused: bool,
}

/// A notice to push to one phone now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub device: String,
    /// Which of the devices with that id: one id can be paired on the
    /// network and through the relay.
    pub door: Door,
    pub sub: push::Subscription,
    pub payload: String,
}

struct Pending {
    kind: Kind,
    payload: String,
    made: Instant,
}

/// What `Notifier` does with a notice for one phone.
enum Fate {
    Send,
    Wait,
    Drop,
}

/// Which of a session's waiting notices a newer one replaces. A turn's end
/// can come while a question or a permission is still open (a subagent's),
/// so the end must not take the question's place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    /// `input`: the session waits for the person.
    Input,
    /// `done` or `error`: how the turn ended.
    End,
}

impl Slot {
    fn of(kind: Kind) -> Slot {
        match kind {
            Kind::Input => Slot::Input,
            Kind::Done | Kind::Error => Slot::End,
        }
    }
}

/// Decides which phones get a notice and when: at once, once the person is
/// away from the computer, or never. Pure: the caller brings the time.
#[derive(Default)]
pub struct Notifier {
    /// At most one per (device id, door, session, slot); a newer one
    /// replaces it.
    pending: BTreeMap<(String, Door, String, Slot), Pending>,
}

impl Notifier {
    /// A session has news. Returns the notices to send now and queues the
    /// ones that wait for the person to leave.
    pub fn offer(
        &mut self,
        c: Candidate,
        devices: &[Device],
        looking: &HashSet<String>,
        p: Presence,
        now: Instant,
    ) -> Vec<Delivery> {
        let mut out = Vec::new();
        for d in devices {
            // The newest notice of its slot wins, even one that stays quiet:
            // the older one is about a state the session has left. The other
            // slot's stays; `tick` drops it once the session moved on.
            let door = Door::of(d);
            let key = (d.id.clone(), door, c.session.clone(), Slot::of(c.kind));
            self.pending.remove(&key);
            let Some(sub) = &d.push else { continue };
            if !wants(&d.notices, c.kind) || looking.contains(&d.id) {
                continue;
            }
            let short = c
                .turn_secs
                .is_some_and(|t| t < u64::from(d.notices.min_turn_secs));
            if c.kind == Kind::Done && short {
                continue;
            }
            match fate(&d.notices, c.kind, p, None) {
                Fate::Send => out.push(Delivery {
                    device: d.id.clone(),
                    door,
                    sub: sub.clone(),
                    payload: c.payload.clone(),
                }),
                Fate::Wait => {
                    self.pending.insert(
                        key,
                        Pending {
                            kind: c.kind,
                            payload: c.payload.clone(),
                            made: now,
                        },
                    );
                }
                Fate::Drop => {}
            }
        }
        out
    }

    /// Checks the queue: sends what may go now and drops what no longer
    /// matters. `still(session, kind)` says whether the session is still in
    /// the state the notice is about.
    pub fn tick(
        &mut self,
        devices: &[Device],
        looking: &HashSet<String>,
        p: Presence,
        still: impl Fn(&str, Kind) -> bool,
        now: Instant,
    ) -> Vec<Delivery> {
        let mut out = Vec::new();
        self.pending.retain(|(id, door, session, _), item| {
            // The phone as it is now: its subscription may have changed.
            let Some(d) = devices.iter().find(|d| &d.id == id && door.admits(d)) else {
                return false;
            };
            let Some(sub) = &d.push else { return false };
            if !wants(&d.notices, item.kind) || looking.contains(id) {
                return false;
            }
            if !still(session, item.kind) {
                return false;
            }
            let waited = now.saturating_duration_since(item.made);
            match fate(&d.notices, item.kind, p, Some(waited)) {
                Fate::Send => {
                    out.push(Delivery {
                        device: id.clone(),
                        door: *door,
                        sub: sub.clone(),
                        payload: item.payload.clone(),
                    });
                    false
                }
                Fate::Wait => true,
                Fate::Drop => false,
            }
        });
        out
    }

    /// The person opened the session on the computer.
    pub fn seen(&mut self, session: &str) {
        self.pending.retain(|(_, _, s, _), _| s != session);
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}

fn wants(n: &Notices, kind: Kind) -> bool {
    match kind {
        Kind::Done => n.done,
        Kind::Input => n.input,
        Kind::Error => n.error,
    }
}

/// The away rule for one phone. At the computer, `input` waits for the
/// person to leave; `done` and `error` were seen there. When the host can't
/// tell idle time, the notice waits the away time in an unfocused window
/// instead; `waited` is `None` for a notice just made.
fn fate(n: &Notices, kind: Kind, p: Presence, waited: Option<Duration>) -> Fate {
    let away = Duration::from_secs(n.away_after_secs.into());
    if away.is_zero() {
        return Fate::Send;
    }
    match p.idle {
        Some(idle) if idle >= away => Fate::Send,
        Some(_) if kind == Kind::Input => Fate::Wait,
        Some(_) => Fate::Drop,
        None if !p.focused && waited.is_some_and(|w| w >= away) => Fate::Send,
        None => Fate::Wait,
    }
}

const FALLBACK: &str = "Finished its turn.";
/// Lines of a plan a notice shows.
const PLAN_LINES: usize = 3;

/// The last thing the agent said in the last turn, as plain text.
/// `push::notice` cuts it to fit. An answer from before the last prompt is
/// the previous turn's, so the search stops there.
pub fn finished_body(entries: &[Entry]) -> String {
    let turn = entries
        .iter()
        .rposition(|e| matches!(e, Entry::User { .. }))
        .map_or(entries, |at| &entries[at + 1..]);
    let text = turn.iter().rev().find_map(|e| match e {
        Entry::Agent { text } => Some(plain(text)),
        _ => None,
    });
    match text {
        Some(t) if !t.is_empty() => t,
        _ => FALLBACK.to_string(),
    }
}

/// What the agent asks leave for: the plan, or the tool and what it works on.
pub fn permission_body(p: &PermissionRequest, tool: Option<&ToolCall>) -> String {
    if let Some(plan) = &p.plan {
        let lines: Vec<&str> = plan
            .plan
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(PLAN_LINES)
            .collect();
        return format!("Plan ready: {}", lines.join(" / "));
    }
    let title = tool
        .map(|t| t.title.as_str())
        .filter(|t| !t.is_empty())
        .unwrap_or(&p.title);
    match tool.and_then(subject) {
        Some(s) => format!("{title}: {s}"),
        None => title.to_string(),
    }
}

/// What a session waiting for the user says: the permission it asks, with the
/// tool it is for found in `entries`, or its question. Both senders use it.
pub fn input_body(
    entries: &[Entry],
    permission: Option<&PermissionRequest>,
    question: Option<&AskUserRequest>,
) -> String {
    if let Some(p) = permission {
        let tool = entries.iter().find_map(|e| match e {
            Entry::Tool(tc) if tc.id == p.tool_call_id => Some(tc),
            _ => None,
        });
        return permission_body(p, tool);
    }
    match question.map(question_body) {
        Some(text) if !text.is_empty() => text,
        _ => "Needs your input.".into(),
    }
}

/// The first question, and how many more there are.
pub fn question_body(q: &AskUserRequest) -> String {
    let Some(first) = q.questions.first() else {
        return String::new();
    };
    let text = plain(&first.question);
    match q.questions.len() - 1 {
        0 => text,
        more => format!("{text} (+{more} more)"),
    }
}

/// How long the last turn took, in whole seconds, once it has settled.
pub fn turn_secs(entries: &[Entry]) -> Option<u64> {
    entries.iter().rev().find_map(|e| match e {
        Entry::User { elapsed_ms, .. } => Some(elapsed_ms.map(|ms| ms / 1000)),
        _ => None,
    })?
}

/// The command, address or file a tool call works on.
fn subject(t: &ToolCall) -> Option<String> {
    let input = |key: &str| {
        t.raw_input
            .as_ref()?
            .get(key)?
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match t.kind.as_str() {
        "execute" => input("command"),
        "fetch" => input("url"),
        _ => None,
    }
    .or_else(|| t.locations.first().map(|p| p.display().to_string()))
    .or_else(|| input("file_path"))
    .or_else(|| input("path"))
}

/// Markdown as one line of plain text: no code blocks, links as their text,
/// no emphasis marks.
fn plain(md: &str) -> String {
    let mut kept = String::new();
    let mut fenced = false;
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    let mut out = String::new();
    let mut rest = kept.as_str();
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let link = after
            .find("](")
            .filter(|&i| !after[..i].contains(['[', '\n']))
            .and_then(|i| after[i + 2..].find(')').map(|j| (i, i + 2 + j)));
        match link {
            Some((text_end, url_end)) => {
                out.push_str(&after[..text_end]);
                rest = &after[url_end + 1..];
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    // `*` and `_` inside a word (`my_file.rs`, `a*b`) are part of it, not emphasis.
    let chars: Vec<char> = out.chars().collect();
    let out: String = chars
        .iter()
        .enumerate()
        .map(|(i, &c)| match c {
            '#' | '`' => ' ',
            '*' | '_' => {
                let word = |j: Option<usize>| {
                    j.and_then(|j| chars.get(j))
                        .is_some_and(|c| c.is_alphanumeric())
                };
                if word(i.checked_sub(1)) && word(Some(i + 1)) {
                    c
                } else {
                    ' '
                }
            }
            c => c,
        })
        .collect();
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Question, SpecPlan, ToolStatus};
    use crate::phone::{Door, push};

    fn user(elapsed_ms: Option<u64>) -> Entry {
        Entry::User {
            text: "go".into(),
            at: 0,
            elapsed_ms,
            message_id: None,
            tokens: None,
            shell_changes: Vec::new(),
        }
    }

    fn agent(text: &str) -> Entry {
        Entry::Agent { text: text.into() }
    }

    fn call(kind: &str, title: &str) -> ToolCall {
        ToolCall {
            id: "t".into(),
            title: title.into(),
            kind: kind.into(),
            status: ToolStatus::Pending,
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: None,
            raw_output: None,
            added: 0,
            removed: 0,
            terminal_output: String::new(),
            terminal_dropped_lines: 0,
            exit_code: None,
            parent: None,
            tool_name: None,
        }
    }

    fn permission(title: &str, plan: Option<SpecPlan>) -> PermissionRequest {
        PermissionRequest::for_test(title, plan)
    }

    #[test]
    fn finished_body_takes_last_agent_text() {
        let entries = [
            user(None),
            agent("**Fixed** the `login` bug.\n\nSee [PR](http://x)."),
        ];
        assert_eq!(finished_body(&entries), "Fixed the login bug. See PR.");
    }

    #[test]
    fn finished_body_says_nothing_of_the_previous_turn() {
        let entries = [user(None), agent("Old answer."), user(None)];
        assert_eq!(finished_body(&entries), "Finished its turn.");
        let entries = [user(None), agent("Old answer."), user(None), agent("New.")];
        assert_eq!(finished_body(&entries), "New.");
    }

    #[test]
    fn emphasis_marks_go_but_word_characters_stay() {
        assert_eq!(
            finished_body(&[agent("Edited my_file.rs and **fixed** _it_")]),
            "Edited my_file.rs and fixed it"
        );
        assert_eq!(
            finished_body(&[agent("snake_case a*b *it* __b__")]),
            "snake_case a*b it b"
        );
    }

    #[test]
    fn finished_body_falls_back_and_fits() {
        assert_eq!(finished_body(&[user(None)]), "Finished its turn.");
        assert_eq!(
            finished_body(&[agent("```\nlet x = 1;\n```")]),
            "Finished its turn."
        );
        let body = finished_body(&[agent(&"é".repeat(5000))]);
        let out = push::notice("done", "s", "t", &body, "", None);
        assert!(out.len() <= push::MAX_NOTICE);
    }

    #[test]
    fn permission_body_shows_subject() {
        let p = permission("Run", None);
        let mut t = call("execute", "Run");
        t.raw_input = Some(serde_json::json!({"command": "cargo test"}));
        assert_eq!(permission_body(&p, Some(&t)), "Run: cargo test");

        let mut t = call("edit", "Edit file");
        t.locations = vec!["/a/b.rs".into()];
        assert_eq!(permission_body(&p, Some(&t)), "Edit file: /a/b.rs");

        let mut t = call("fetch", "Fetch");
        t.raw_input = Some(serde_json::json!({"url": "https://x"}));
        assert_eq!(permission_body(&p, Some(&t)), "Fetch: https://x");

        assert_eq!(permission_body(&p, None), "Run");

        let plan = SpecPlan {
            title: "Plan".into(),
            plan: "\n# One\n\ntwo\nthree\nfour\n".into(),
        };
        let p = permission("Exit spec mode", Some(plan));
        assert_eq!(permission_body(&p, None), "Plan ready: # One / two / three");
    }

    #[test]
    fn question_body_first_and_more() {
        let q = |text: &str| Question {
            index: 1,
            topic: String::new(),
            question: text.into(),
            options: Vec::new(),
            multi: false,
        };
        let req = AskUserRequest::for_test(vec![q("Which database?"), q("Which port?")]);
        assert_eq!(question_body(&req), "Which database? (+1 more)");
    }

    #[test]
    fn turn_secs_from_last_user() {
        let entries = [user(Some(1000)), agent("x"), user(Some(65_400))];
        assert_eq!(turn_secs(&entries), Some(65));
        // The last turn has not settled: an earlier turn's time is not its own.
        let entries = [user(Some(1000)), agent("x"), user(None)];
        assert_eq!(turn_secs(&entries), None);
        assert_eq!(turn_secs(&[agent("x")]), None);
    }

    #[test]
    fn input_body_finds_the_tool_and_falls_back() {
        let mut p = permission("Run command", None);
        p.tool_call_id = "t".into();
        let mut tc = call("execute", "Run command");
        tc.raw_input = Some(serde_json::json!({ "command": "cargo test" }));
        let entries = [Entry::Tool(tc)];
        assert_eq!(
            input_body(&entries, Some(&p), None),
            "Run command: cargo test"
        );
        assert_eq!(input_body(&[], None, None), "Needs your input.");
        let empty = AskUserRequest::for_test(Vec::new());
        assert_eq!(input_body(&[], None, Some(&empty)), "Needs your input.");
    }

    fn device(id: &str, notices: Notices) -> Device {
        Device {
            id: id.into(),
            name: id.into(),
            token_hash: String::new(),
            ticket_hash: String::new(),
            paired_at: 0,
            last_seen: 0,
            push: Some(push::Subscription {
                endpoint: format!("https://push.example/{id}"),
                p256dh: "k".into(),
                auth: "a".into(),
            }),
            notices,
        }
    }

    fn phone() -> Device {
        device("p", Notices::default())
    }

    fn cand(session: &str, kind: Kind, turn_secs: Option<u64>) -> Candidate {
        Candidate {
            session: session.into(),
            kind,
            turn_secs,
            payload: format!(
                "{{\"session\":\"{session}\",\"kind\":\"{}\"}}",
                kind.as_str()
            ),
        }
    }

    fn idle(secs: u64) -> Presence {
        Presence {
            idle: Some(Duration::from_secs(secs)),
            focused: false,
        }
    }

    fn unknown(focused: bool) -> Presence {
        Presence {
            idle: None,
            focused,
        }
    }

    fn nobody() -> HashSet<String> {
        HashSet::new()
    }

    fn always(_: &str, _: Kind) -> bool {
        true
    }

    #[test]
    fn away_zero_sends_now() {
        let d = device(
            "p",
            Notices {
                away_after_secs: 0,
                ..Notices::default()
            },
        );
        let mut n = Notifier::default();
        let now = Instant::now();
        for kind in [Kind::Done, Kind::Input, Kind::Error] {
            for p in [idle(0), unknown(true)] {
                let out = n.offer(
                    cand("s", kind, None),
                    std::slice::from_ref(&d),
                    &nobody(),
                    p,
                    now,
                );
                assert_eq!(out.len(), 1, "{kind:?} {p:?}");
                assert_eq!(out[0].device, "p");
                assert_eq!(out[0].sub, d.push.clone().unwrap());
                assert_eq!(out[0].payload, cand("s", kind, None).payload);
            }
        }
        assert!(!n.has_pending());
    }

    #[test]
    fn idle_known_away_sends() {
        let mut n = Notifier::default();
        let now = Instant::now();
        for kind in [Kind::Done, Kind::Input, Kind::Error] {
            let out = n.offer(cand("s", kind, None), &[phone()], &nobody(), idle(120), now);
            assert_eq!(out.len(), 1, "{kind:?}");
        }
        assert!(!n.has_pending());
    }

    #[test]
    fn idle_known_present_drops_done_and_error() {
        let mut n = Notifier::default();
        let now = Instant::now();
        for kind in [Kind::Done, Kind::Error] {
            let out = n.offer(cand("s", kind, None), &[phone()], &nobody(), idle(5), now);
            assert!(out.is_empty(), "{kind:?}");
        }
        assert!(!n.has_pending());
        let later = now + Duration::from_secs(600);
        assert!(
            n.tick(&[phone()], &nobody(), idle(600), always, later)
                .is_empty()
        );
    }

    #[test]
    fn idle_known_present_queues_input_until_away() {
        let mut n = Notifier::default();
        let now = Instant::now();
        let out = n.offer(
            cand("s", Kind::Input, None),
            &[phone()],
            &nobody(),
            idle(5),
            now,
        );
        assert!(out.is_empty());
        assert!(n.has_pending());
        let soon = now + Duration::from_secs(15);
        assert!(
            n.tick(&[phone()], &nobody(), idle(20), always, soon)
                .is_empty()
        );
        assert!(n.has_pending(), "still at the computer");
        let later = now + Duration::from_secs(130);
        let out = n.tick(&[phone()], &nobody(), idle(120), always, later);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].payload, cand("s", Kind::Input, None).payload);
        assert!(!n.has_pending());
    }

    #[test]
    fn tick_drops_input_no_longer_waiting() {
        let mut n = Notifier::default();
        let now = Instant::now();
        n.offer(
            cand("s", Kind::Input, None),
            &[phone()],
            &nobody(),
            idle(5),
            now,
        );
        let answered = |s: &str, k: Kind| !(s == "s" && k == Kind::Input);
        let later = now + Duration::from_secs(300);
        assert!(
            n.tick(&[phone()], &nobody(), idle(300), answered, later)
                .is_empty()
        );
        assert!(!n.has_pending());
    }

    #[test]
    fn idle_unknown_waits_then_sends_if_unfocused() {
        let mut n = Notifier::default();
        let now = Instant::now();
        let out = n.offer(
            cand("s", Kind::Done, None),
            &[phone()],
            &nobody(),
            unknown(false),
            now,
        );
        assert!(out.is_empty());
        assert!(n.has_pending());
        let soon = now + Duration::from_secs(119);
        assert!(
            n.tick(&[phone()], &nobody(), unknown(false), always, soon)
                .is_empty()
        );
        let later = now + Duration::from_secs(120);
        let out = n.tick(&[phone()], &nobody(), unknown(false), always, later);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].payload, cand("s", Kind::Done, None).payload);
        assert!(!n.has_pending());
    }

    #[test]
    fn idle_unknown_focused_keeps_waiting() {
        let mut n = Notifier::default();
        let now = Instant::now();
        n.offer(
            cand("s", Kind::Error, None),
            &[phone()],
            &nobody(),
            unknown(true),
            now,
        );
        let later = now + Duration::from_secs(600);
        assert!(
            n.tick(&[phone()], &nobody(), unknown(true), always, later)
                .is_empty()
        );
        assert!(n.has_pending());
        let out = n.tick(&[phone()], &nobody(), unknown(false), always, later);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn seen_drops_pending() {
        let mut n = Notifier::default();
        let now = Instant::now();
        n.offer(
            cand("s", Kind::Input, None),
            &[phone()],
            &nobody(),
            unknown(false),
            now,
        );
        n.offer(
            cand("t", Kind::Input, None),
            &[phone()],
            &nobody(),
            unknown(false),
            now,
        );
        n.seen("s");
        let later = now + Duration::from_secs(600);
        let out = n.tick(&[phone()], &nobody(), unknown(false), always, later);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].payload, cand("t", Kind::Input, None).payload);
        n.offer(
            cand("s", Kind::Input, None),
            &[phone()],
            &nobody(),
            unknown(false),
            now,
        );
        n.seen("s");
        assert!(!n.has_pending());
    }

    #[test]
    fn short_turn_done_dropped_input_kept() {
        let mut n = Notifier::default();
        let now = Instant::now();
        let away = idle(600);
        let short = n.offer(
            cand("s", Kind::Done, Some(29)),
            &[phone()],
            &nobody(),
            away,
            now,
        );
        assert!(short.is_empty());
        let long = n.offer(
            cand("s", Kind::Done, Some(30)),
            &[phone()],
            &nobody(),
            away,
            now,
        );
        assert_eq!(long.len(), 1);
        let unknown_len = n.offer(
            cand("s", Kind::Done, None),
            &[phone()],
            &nobody(),
            away,
            now,
        );
        assert_eq!(unknown_len.len(), 1, "an unknown length counts as long");
        let input = n.offer(
            cand("s", Kind::Input, Some(1)),
            &[phone()],
            &nobody(),
            away,
            now,
        );
        assert_eq!(input.len(), 1);
        let error = n.offer(
            cand("s", Kind::Error, Some(1)),
            &[phone()],
            &nobody(),
            away,
            now,
        );
        assert_eq!(error.len(), 1);
        let every = device(
            "p",
            Notices {
                min_turn_secs: 0,
                ..Notices::default()
            },
        );
        let out = n.offer(
            cand("s", Kind::Done, Some(0)),
            &[every],
            &nobody(),
            away,
            now,
        );
        assert_eq!(out.len(), 1, "0 turns the rule off");
    }

    #[test]
    fn newer_replaces_pending() {
        let mut n = Notifier::default();
        let now = Instant::now();
        for (older, newer) in [
            (Kind::Input, Kind::Input),
            (Kind::Error, Kind::Done),
            (Kind::Done, Kind::Error),
        ] {
            n.offer(
                cand("s", older, None),
                &[phone()],
                &nobody(),
                unknown(false),
                now,
            );
            let mut c = cand("s", newer, None);
            c.payload = "newer".into();
            n.offer(
                c,
                &[phone()],
                &nobody(),
                unknown(false),
                now + Duration::from_secs(60),
            );
            // The newer one's own wait counts, not the older one's.
            let at_old = now + Duration::from_secs(120);
            assert!(
                n.tick(&[phone()], &nobody(), unknown(false), always, at_old)
                    .is_empty()
            );
            let at_new = now + Duration::from_secs(180);
            let out = n.tick(&[phone()], &nobody(), unknown(false), always, at_new);
            assert_eq!(out.len(), 1, "{older:?} then {newer:?}");
            assert_eq!(out[0].payload, "newer");
            assert!(!n.has_pending());
        }
    }

    #[test]
    fn looking_device_skipped() {
        let mut n = Notifier::default();
        let now = Instant::now();
        let devices = [phone(), device("q", Notices::default())];
        let looking: HashSet<String> = ["p".to_string()].into();
        let out = n.offer(
            cand("s", Kind::Done, None),
            &devices,
            &looking,
            idle(600),
            now,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].device, "q");
        // A pending one goes too once its phone shows the session.
        n.offer(
            cand("t", Kind::Input, None),
            &devices,
            &nobody(),
            unknown(false),
            now,
        );
        let later = now + Duration::from_secs(600);
        let out = n.tick(&devices, &looking, unknown(false), always, later);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].device, "q");
        assert!(!n.has_pending());
    }

    #[test]
    fn skips_kinds_switched_off_and_phones_without_a_subscription() {
        let mut n = Notifier::default();
        let now = Instant::now();
        let mut unsubscribed = device("u", Notices::default());
        unsubscribed.push = None;
        let no_done = device(
            "d",
            Notices {
                done: false,
                ..Notices::default()
            },
        );
        let devices = [unsubscribed, no_done];
        let out = n.offer(
            cand("s", Kind::Done, None),
            &devices,
            &nobody(),
            idle(600),
            now,
        );
        assert!(out.is_empty());
        let out = n.offer(
            cand("s", Kind::Input, None),
            &devices,
            &nobody(),
            idle(600),
            now,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].device, "d");
    }

    #[test]
    fn tick_uses_the_current_subscription() {
        let mut n = Notifier::default();
        let now = Instant::now();
        n.offer(
            cand("s", Kind::Input, None),
            &[phone()],
            &nobody(),
            unknown(false),
            now,
        );
        n.offer(
            cand("s", Kind::Input, None),
            &[device("q", Notices::default())],
            &nobody(),
            unknown(false),
            now,
        );
        let mut moved = phone();
        moved.push.as_mut().unwrap().endpoint = "https://push.example/new".into();
        let mut gone = device("q", Notices::default());
        gone.push = None;
        let later = now + Duration::from_secs(600);
        let out = n.tick(&[moved, gone], &nobody(), unknown(false), always, later);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sub.endpoint, "https://push.example/new");
        assert!(!n.has_pending());
    }

    #[test]
    fn tick_with_idle_now_known_drops_done_at_the_computer() {
        let mut n = Notifier::default();
        let now = Instant::now();
        n.offer(
            cand("s", Kind::Done, None),
            &[phone()],
            &nobody(),
            unknown(false),
            now,
        );
        n.offer(
            cand("t", Kind::Input, None),
            &[phone()],
            &nobody(),
            unknown(false),
            now,
        );
        let later = now + Duration::from_secs(600);
        assert!(
            n.tick(&[phone()], &nobody(), idle(5), always, later)
                .is_empty()
        );
        assert!(n.has_pending(), "the input waits");
        let out = n.tick(&[phone()], &nobody(), idle(120), always, later);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].payload, cand("t", Kind::Input, None).payload);
    }

    /// The same id paired on the network (with a token) and through the relay.
    fn twins(network: Notices, relay: Notices) -> [Device; 2] {
        let mut lan = device("same", network);
        lan.token_hash = "hash".into();
        lan.push.as_mut().unwrap().endpoint = "https://push.example/lan".into();
        let mut rel = device("same", relay);
        rel.push.as_mut().unwrap().endpoint = "https://push.example/relay".into();
        [lan, rel]
    }

    #[test]
    fn a_twin_without_a_subscription_does_not_lose_the_other_ones_notice() {
        let [mut lan, rel] = twins(Notices::default(), Notices::default());
        lan.push = None;
        let devices = [lan, rel];
        let mut n = Notifier::default();
        let now = Instant::now();
        let out = n.offer(
            cand("s", Kind::Input, None),
            &devices,
            &nobody(),
            idle(5),
            now,
        );
        assert!(out.is_empty());
        let later = now + Duration::from_secs(200);
        let out = n.tick(&devices, &nobody(), idle(200), always, later);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sub.endpoint, "https://push.example/relay");
        assert_eq!(out[0].door, Door::Relay);
        assert!(!n.has_pending());
    }

    #[test]
    fn twins_each_follow_their_own_away_time() {
        let at_once = Notices {
            away_after_secs: 0,
            ..Notices::default()
        };
        let devices = twins(at_once, Notices::default());
        let mut n = Notifier::default();
        let now = Instant::now();
        let out = n.offer(
            cand("s", Kind::Input, None),
            &devices,
            &nobody(),
            idle(5),
            now,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sub.endpoint, "https://push.example/lan");
        assert_eq!(out[0].door, Door::Local);
        let soon = now + Duration::from_secs(15);
        assert!(
            n.tick(&devices, &nobody(), idle(20), always, soon)
                .is_empty()
        );
        let later = now + Duration::from_secs(130);
        let out = n.tick(&devices, &nobody(), idle(120), always, later);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sub.endpoint, "https://push.example/relay");
        assert_eq!(out[0].door, Door::Relay);
        let end = now + Duration::from_secs(300);
        assert!(
            n.tick(&devices, &nobody(), idle(300), always, end)
                .is_empty()
        );
        assert!(!n.has_pending());
    }

    #[test]
    fn away_zero_sends_at_once_in_tick() {
        let mut n = Notifier::default();
        let now = Instant::now();
        n.offer(
            cand("s", Kind::Input, None),
            &[phone()],
            &nobody(),
            idle(5),
            now,
        );
        assert!(n.has_pending());
        let at_once = device(
            "p",
            Notices {
                away_after_secs: 0,
                ..Notices::default()
            },
        );
        let soon = now + Duration::from_secs(15);
        let out = n.tick(&[at_once], &nobody(), idle(20), always, soon);
        assert_eq!(out.len(), 1);
        assert!(!n.has_pending());
    }

    #[test]
    fn a_skipped_newer_notice_still_replaces_the_pending_one() {
        let mut n = Notifier::default();
        let now = Instant::now();
        n.offer(
            cand("s", Kind::Done, Some(300)),
            &[phone()],
            &nobody(),
            unknown(false),
            now,
        );
        let short = n.offer(
            cand("s", Kind::Done, Some(1)),
            &[phone()],
            &nobody(),
            unknown(false),
            now,
        );
        assert!(short.is_empty());
        assert!(
            !n.has_pending(),
            "the newest notice wins, even one that stays quiet"
        );
    }

    #[test]
    fn a_turns_end_leaves_a_waiting_question() {
        let mut n = Notifier::default();
        let now = Instant::now();
        let asking = || cand("s", Kind::Input, None);
        // A short turn's end stays quiet but takes nothing away.
        n.offer(asking(), &[phone()], &nobody(), unknown(false), now);
        let short = n.offer(
            cand("s", Kind::Done, Some(1)),
            &[phone()],
            &nobody(),
            unknown(false),
            now,
        );
        assert!(short.is_empty());
        let later = now + Duration::from_secs(120);
        let out = n.tick(&[phone()], &nobody(), unknown(false), always, later);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].payload, asking().payload);

        // An end that goes at once goes, and the question still waits.
        n.offer(asking(), &[phone()], &nobody(), idle(10), now);
        let failed = n.offer(
            cand("s", Kind::Error, None),
            &[phone()],
            &nobody(),
            idle(600),
            now,
        );
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].payload, cand("s", Kind::Error, None).payload);
        assert!(n.has_pending(), "the question waits");
        let out = n.tick(&[phone()], &nobody(), idle(600), always, later);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].payload, asking().payload);

        // Once it is answered, it goes without a word.
        n.offer(asking(), &[phone()], &nobody(), idle(10), now);
        n.offer(
            cand("s", Kind::Done, Some(300)),
            &[phone()],
            &nobody(),
            idle(600),
            now,
        );
        let answered = |_: &str, k: Kind| k != Kind::Input;
        assert!(
            n.tick(&[phone()], &nobody(), idle(600), answered, later)
                .is_empty()
        );
        assert!(!n.has_pending());
    }
}
