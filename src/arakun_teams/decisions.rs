//! The team's decision log, read back from decrypted channel history: every
//! decision @bot proposed that a member accepted with a `decision_accept`
//! message, with who accepted it and when, the range it came from and what
//! the bot checked. Nothing about it is stored apart from the messages.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::bot::{self, Verified};
use super::hub::{Delivery, Hub, Msg};

/// One member accepting a decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Acceptance {
    pub github_id: u64,
    pub login: String,
    pub at: u64,
    /// The `decision_accept` message.
    pub message_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    /// The bot message that proposed it.
    pub bot_id: String,
    pub channel_id: String,
    /// A short name: the decision's first sentence.
    pub title: String,
    pub decision: String,
    pub proposed_at: u64,
    /// Whose Claude Code proposed it.
    pub via: u64,
    pub via_login: String,
    /// The first and last message the bot read.
    pub range: [String; 2],
    /// How many messages that was, when the bot said.
    pub read: Option<usize>,
    /// How many parts a long range was summed up in.
    pub parts: Option<usize>,
    /// `[n]` → message ID.
    pub cites: BTreeMap<usize, String>,
    pub verified: Vec<Verified>,
    /// Oldest first, one per member.
    pub accepted: Vec<Acceptance>,
}

impl Decision {
    /// When the first member accepted it.
    pub fn decided_at(&self) -> u64 {
        self.accepted.first().map_or(self.proposed_at, |a| a.at)
    }

    pub fn accepted_by(&self, github_id: u64) -> bool {
        self.accepted.iter().any(|a| a.github_id == github_id)
    }
}

/// The bot message a `decision_accept` accepts.
pub fn accepts(m: &Msg) -> Option<&str> {
    if m.body.kind != "decision_accept" || matches!(m.delivery, Delivery::Failed(_)) {
        return None;
    }
    m.body.decision.as_ref()?.get("of")?.as_str()
}

/// Who accepted the bot message `bot_id`, oldest first, once per member.
pub fn acceptances(messages: &[Msg], bot_id: &str) -> Vec<Acceptance> {
    let mut seen = HashSet::new();
    messages
        .iter()
        .filter(|m| accepts(m) == Some(bot_id))
        .filter(|m| seen.insert(m.github_id))
        .map(|m| Acceptance {
            github_id: m.github_id,
            login: m.login.clone(),
            at: m.at,
            message_id: m.id.clone(),
        })
        .collect()
}

/// A decision's first sentence, cut to a line.
pub fn title_of(decision: &str) -> String {
    const MAX: usize = 80;
    let first = decision
        .split_inclusive(['.', '!', '?', '\n'])
        .next()
        .unwrap_or(decision)
        .trim()
        .trim_end_matches('.');
    if first.chars().count() <= MAX {
        return first.to_string();
    }
    let cut: String = first.chars().take(MAX).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(a, _)| a);
    format!("{}…", cut.trim_end())
}

/// The accepted decisions of one channel, in the order they were proposed.
/// Messages are in the channel's order, as the hub keeps them.
pub fn channel_log(channel_id: &str, messages: &[Msg]) -> Vec<Decision> {
    let mut accepted: HashMap<&str, Vec<Acceptance>> = HashMap::new();
    let mut seen: HashSet<(&str, u64)> = HashSet::new();
    for m in messages {
        if let Some(of) = accepts(m)
            && seen.insert((of, m.github_id))
        {
            accepted.entry(of).or_default().push(Acceptance {
                github_id: m.github_id,
                login: m.login.clone(),
                at: m.at,
                message_id: m.id.clone(),
            });
        }
    }
    messages
        .iter()
        .filter(|m| m.delivery == Delivery::Sent)
        .filter_map(|m| {
            let accepted = accepted.remove(m.id.as_str())?;
            let meta = bot::meta(m)?;
            let answer = meta.answer?;
            let cites = meta
                .cites
                .into_iter()
                .filter_map(|(n, id)| Some((n.parse().ok()?, id)))
                .collect();
            Some(Decision {
                bot_id: m.id.clone(),
                channel_id: channel_id.to_string(),
                title: title_of(&answer.decision),
                decision: answer.decision,
                proposed_at: m.at,
                via: meta.via,
                via_login: m.login.clone(),
                range: meta.range,
                read: meta.read,
                parts: meta.parts,
                cites,
                verified: answer.verified,
                accepted,
            })
        })
        .collect()
}

/// Every channel's accepted decisions, the most recently decided first.
pub fn team_log<'a>(channels: impl IntoIterator<Item = (&'a str, &'a [Msg])>) -> Vec<Decision> {
    let mut all: Vec<Decision> = channels
        .into_iter()
        .flat_map(|(id, messages)| channel_log(id, messages))
        .collect();
    all.sort_by_key(|d| std::cmp::Reverse(d.decided_at()));
    all
}

impl Hub {
    /// The decision log of a team, from the channels open on this device.
    pub fn decision_log(&self, team_id: &str) -> Vec<Decision> {
        team_log(
            self.channels
                .values()
                .filter(|c| c.team_id == team_id)
                .map(|c| (c.channel_id.as_str(), c.messages.as_slice())),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arakun_teams::bot::{Answer, Idea, Outcome, Range, Verdict, accept_body, bot_body};
    use crate::arakun_teams::crypto::Body;

    fn msg(id: &str, at: u64, gid: u64, login: &str, body: Body) -> Msg {
        Msg {
            id: id.into(),
            seq: Some(at),
            at,
            github_id: gid,
            login: login.into(),
            body,
            delivery: Delivery::Sent,
            edited: None,
        }
    }

    fn answer(decision: &str) -> Answer {
        Answer {
            ideas: vec![Idea {
                label: "A".into(),
                title: "Endorse".into(),
                verdict: Verdict::Works,
                reason: "It's in the protocol.".into(),
                cites: vec![1, 2],
            }],
            verified: vec![Verified {
                text: "Endorsements".into(),
                source: "PROTOCOL.md".into(),
            }],
            assumed: vec![],
            unchecked: vec![],
            decision: decision.into(),
        }
    }

    /// A channel where karlo's @bot proposed a decision in `b1`, then dani
    /// and karlo accepted it, dani twice.
    fn channel() -> Vec<Msg> {
        let range = Range {
            lines: ["m1", "m2"]
                .iter()
                .enumerate()
                .map(|(i, id)| bot::Line {
                    n: i + 1,
                    id: id.to_string(),
                    login: "dani".into(),
                    kind: "text".into(),
                    text: "…".into(),
                    reply_to: None,
                })
                .collect(),
        };
        let outcome = Outcome {
            answer: answer("Use A. Fall back to B when no device is left."),
            parts: 0,
        };
        let bot = msg("b1", 10, 1, "karlo", bot_body(&range, 1, &outcome));
        let a1 = msg("a1", 11, 2, "dani", accept_body(&bot));
        let a2 = msg("a2", 12, 1, "karlo", accept_body(&bot));
        let a3 = msg("a3", 13, 2, "dani", accept_body(&bot));
        vec![
            msg("m1", 1, 2, "dani", Body::text("Idea A", None)),
            msg("m2", 2, 2, "dani", Body::text("@bot", Some("m1".into()))),
            bot,
            a1,
            a2,
            a3,
        ]
    }

    #[test]
    fn builds_the_log_from_accepted_bot_decisions() {
        let log = channel_log("c1", &channel());
        assert_eq!(log.len(), 1);
        let d = &log[0];
        assert_eq!(d.bot_id, "b1");
        assert_eq!(d.channel_id, "c1");
        assert_eq!(d.title, "Use A");
        assert_eq!(d.decision, "Use A. Fall back to B when no device is left.");
        assert_eq!((d.via, d.via_login.as_str()), (1, "karlo"));
        assert_eq!(d.range, ["m1".to_string(), "m2".to_string()]);
        assert_eq!(d.read, Some(2));
        assert_eq!(
            d.cites,
            BTreeMap::from([(1, "m1".into()), (2, "m2".into())])
        );
        assert_eq!(d.verified[0].source, "PROTOCOL.md");
        // One acceptance per member, oldest first.
        let who: Vec<(&str, u64)> = d
            .accepted
            .iter()
            .map(|a| (a.login.as_str(), a.at))
            .collect();
        assert_eq!(who, [("dani", 11), ("karlo", 12)]);
        assert_eq!(d.decided_at(), 11);
        assert!(d.accepted_by(1) && !d.accepted_by(3));
        assert_eq!(acceptances(&channel(), "b1").len(), 2);
    }

    #[test]
    fn leaves_out_what_was_not_accepted_or_not_the_bots() {
        // Not accepted yet.
        let c = channel();
        assert!(channel_log("c1", &c[..3]).is_empty());
        // A bot message posted by someone other than its `via`.
        let mut forged = channel();
        forged[2].github_id = 2;
        assert!(channel_log("c1", &forged).is_empty());
        // An accept that never went out.
        let mut failed = channel();
        failed.truncate(4);
        failed[3].delivery = Delivery::Failed("refused".into());
        assert!(channel_log("c1", &failed).is_empty());
        // An accept of a message that isn't a bot's.
        let mut text = channel();
        text[3].body.decision = Some(serde_json::json!({ "of": "m1" }));
        text.truncate(4);
        assert!(channel_log("c1", &text).is_empty());
    }

    #[test]
    fn the_team_log_puts_the_latest_decision_first() {
        let mut later = channel();
        for m in &mut later {
            m.at += 100;
        }
        let first = channel();
        let log = team_log([("c1", first.as_slice()), ("c2", later.as_slice())]);
        let order: Vec<&str> = log.iter().map(|d| d.channel_id.as_str()).collect();
        assert_eq!(order, ["c2", "c1"]);
    }

    #[test]
    fn titles_are_the_first_sentence_on_one_line() {
        assert_eq!(title_of("Use A. Then B."), "Use A");
        assert_eq!(title_of("Ship it"), "Ship it");
        let long = "word ".repeat(40);
        let t = title_of(&long);
        assert!(t.ends_with('…') && t.chars().count() <= 81, "{t}");
    }
}
