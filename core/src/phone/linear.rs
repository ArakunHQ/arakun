//! An issue delegated to Arakun in Linear, as the relay tells this computer
//! on channel 0. The relay holds each delivery until the app takes it over
//! HTTPS, and sends it again after every `claimed`, so one can come twice.

use std::fmt;

use serde_json::Value;

/// The longest agent session, issue or organization id taken.
const MAX_ID: usize = 64;
/// The longest follow-up text kept, in characters.
pub const MAX_BODY: usize = 16_000;
/// The most characters of guidance kept, all rules together. The Worker
/// passes no more; more is dropped whole, as a cut rule could say the opposite.
pub const MAX_GUIDANCE: usize = 16_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryKind {
    /// The issue was delegated: a new agent session.
    Created,
    /// A follow-up message in the agent session.
    Prompt,
    /// The person asked the agent to stop.
    Stop,
}

/// Why the relay's Worker stopped a session when nobody pressed stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// Arakun was removed as the issue's delegate.
    Unassigned,
    /// The issue was canceled, or marked a duplicate.
    Canceled,
    /// A person marked the issue done.
    Completed,
    /// Arakun lost access to the issue's team.
    TeamAccess,
    /// The Arakun app was removed from the workspace.
    Revoked,
}

impl StopReason {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "unassigned" => Self::Unassigned,
            "canceled" => Self::Canceled,
            "completed" => Self::Completed,
            "team_access" => Self::TeamAccess,
            "revoked" => Self::Revoked,
            _ => return None,
        })
    }
}

/// One rule a Linear workspace or team set for agents, as the relay passed it.
#[derive(Clone, PartialEq, Eq)]
pub struct GuidanceRule {
    /// The team that set it, with its parents' names; `None` for the workspace.
    pub team: Option<String>,
    pub body: String,
}

// Like a delivery's body, the rule's text never reaches a log.
impl fmt::Debug for GuidanceRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuidanceRule")
            .field("team", &self.team.is_some())
            .field("body", &"…")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The relay's id for this delivery: 32 hex digits.
    pub id: String,
    pub kind: DeliveryKind,
    pub agent_session: String,
    pub issue: String,
    pub organization: String,
    pub activity: Option<String>,
    /// For a stop nobody pressed, why the Worker sent it.
    pub reason: Option<StopReason>,
    /// For `Created`: the comment that mentioned Arakun, when a mention
    /// started the session. An id only: its text is read from Linear.
    pub comment: Option<String>,
    /// The follow-up's text, when the relay had it at hand.
    pub body: Option<String>,
    /// A new delegation's guidance for agents, when the relay had it at
    /// hand: only in the live frame, never in one sent again.
    pub guidance: Vec<GuidanceRule>,
}

// Text from Linear is data: it never reaches a log, so `Debug` hides it.
impl fmt::Debug for Delivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Delivery")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("agent_session", &self.agent_session)
            .field("issue", &self.issue)
            .field("organization", &self.organization)
            .field("activity", &self.activity)
            .field("reason", &self.reason)
            .field("comment", &self.comment)
            .field("body", &self.body.as_ref().map(|_| "…"))
            .field("guidance", &self.guidance.len())
            .finish()
    }
}

/// The delivery in a `linear` control message, or `None` when any field is
/// missing or not what the relay sends. The relay checks the ids already;
/// this only keeps a broken message from reaching the app.
pub fn parse(msg: &Value) -> Option<Delivery> {
    if msg.get("type")?.as_str()? != "linear" {
        return None;
    }
    let id = msg.get("id")?.as_str()?;
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let kind = match msg.get("kind")?.as_str()? {
        "created" => DeliveryKind::Created,
        "prompt" => DeliveryKind::Prompt,
        "stop" => DeliveryKind::Stop,
        _ => return None,
    };
    let short = |field: &str| {
        let s = msg.get(field)?.as_str()?;
        (!s.is_empty() && s.chars().count() <= MAX_ID).then(|| s.to_string())
    };
    // Absent or null is `None`; any other type spoils the message.
    let optional = |field: &str| -> Option<Option<&str>> {
        match msg.get(field) {
            None | Some(Value::Null) => Some(None),
            Some(Value::String(s)) => Some(Some(s)),
            Some(_) => None,
        }
    };
    let activity = match optional("activity")? {
        Some(a) if a.chars().count() > MAX_ID => return None,
        a => a.map(str::to_string),
    };
    // A reason this build does not know leaves a plain stop: it still stops.
    let reason = match optional("reason")? {
        Some(r) if kind == DeliveryKind::Stop => StopReason::parse(r),
        _ => None,
    };
    // The mention is a bonus: one that is not an id leaves none, and the
    // delivery counts.
    let comment = msg
        .get("comment")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty() && c.chars().count() <= MAX_ID)
        .map(str::to_string);
    // A body too long to trust is left out; the delivery still counts.
    let body = optional("body")?
        .filter(|b| b.chars().count() <= MAX_BODY)
        .map(str::to_string);
    Some(Delivery {
        id: id.to_string(),
        kind,
        agent_session: short("agent_session")?,
        issue: short("issue")?,
        organization: short("organization")?,
        activity,
        reason,
        comment,
        body,
        guidance: guidance(msg.get("guidance")),
    })
}

/// The rules in a frame's `guidance`. Guidance is a bonus: anything not
/// what the relay sends, or too long, leaves none, and the delivery counts.
fn guidance(v: Option<&Value>) -> Vec<GuidanceRule> {
    let Some(Value::Array(list)) = v else {
        return Vec::new();
    };
    let mut rules = Vec::with_capacity(list.len());
    let mut size = 0;
    for r in list {
        let Some(body) = r.get("body").and_then(Value::as_str) else {
            return Vec::new();
        };
        let team = match r.get("team") {
            None | Some(Value::Null) => None,
            Some(Value::String(t)) => Some(t.clone()),
            Some(_) => return Vec::new(),
        };
        size += body.chars().count() + team.as_deref().map_or(0, |t| t.chars().count());
        if size > MAX_GUIDANCE {
            return Vec::new();
        }
        rules.push(GuidanceRule {
            team,
            body: body.to_string(),
        });
    }
    rules
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    fn full() -> Value {
        json!({
            "type": "linear",
            "id": ID,
            "kind": "prompt",
            "agent_session": "5b0c1f9e-0000-4000-8000-000000000001",
            "issue": "5b0c1f9e-0000-4000-8000-000000000002",
            "organization": "5b0c1f9e-0000-4000-8000-000000000003",
            "activity": "5b0c1f9e-0000-4000-8000-000000000004",
            "body": "Please also fix the tests.",
        })
    }

    fn with(field: &str, value: Value) -> Value {
        let mut msg = full();
        msg[field] = value;
        msg
    }

    fn without(field: &str) -> Value {
        let mut msg = full();
        msg.as_object_mut().unwrap().remove(field);
        msg
    }

    #[test]
    fn a_full_frame_parses() {
        let d = parse(&full()).unwrap();
        assert_eq!(
            d,
            Delivery {
                id: ID.into(),
                kind: DeliveryKind::Prompt,
                agent_session: "5b0c1f9e-0000-4000-8000-000000000001".into(),
                issue: "5b0c1f9e-0000-4000-8000-000000000002".into(),
                organization: "5b0c1f9e-0000-4000-8000-000000000003".into(),
                activity: Some("5b0c1f9e-0000-4000-8000-000000000004".into()),
                reason: None,
                comment: None,
                body: Some("Please also fix the tests.".into()),
                guidance: Vec::new(),
            }
        );
        assert_eq!(
            parse(&with("kind", json!("created"))).unwrap().kind,
            DeliveryKind::Created
        );
        assert_eq!(
            parse(&with("kind", json!("stop"))).unwrap().kind,
            DeliveryKind::Stop
        );
    }

    #[test]
    fn a_stop_carries_its_reason() {
        let stop = |reason: Value| {
            let mut msg = with("kind", json!("stop"));
            msg["reason"] = reason;
            parse(&msg).unwrap().reason
        };
        assert_eq!(stop(json!("unassigned")), Some(StopReason::Unassigned));
        assert_eq!(stop(json!("canceled")), Some(StopReason::Canceled));
        assert_eq!(stop(json!("completed")), Some(StopReason::Completed));
        assert_eq!(stop(json!("team_access")), Some(StopReason::TeamAccess));
        assert_eq!(stop(json!("revoked")), Some(StopReason::Revoked));
        // A reason from a newer Worker still stops, as a plain stop.
        assert_eq!(stop(json!("bored")), None);
        assert_eq!(stop(Value::Null), None);
        assert_eq!(parse(&with("kind", json!("stop"))).unwrap().reason, None);
        // Only a stop has one.
        assert_eq!(
            parse(&with("reason", json!("unassigned"))).unwrap().reason,
            None
        );
    }

    #[test]
    fn activity_and_body_may_be_null_or_absent() {
        for msg in [
            with("activity", Value::Null),
            without("activity"),
            with("body", Value::Null),
            without("body"),
        ] {
            assert!(parse(&msg).is_some(), "{msg}");
        }
        assert_eq!(parse(&without("activity")).unwrap().activity, None);
        assert_eq!(parse(&with("body", Value::Null)).unwrap().body, None);
        assert_eq!(parse(&without("body")).unwrap().body, None);
    }

    #[test]
    fn a_created_frame_names_the_comment_that_mentioned_arakun() {
        let c = "5b0c1f9e-0000-4000-8000-000000000005";
        let msg = with("comment", json!(c));
        assert_eq!(parse(&msg).unwrap().comment.as_deref(), Some(c));
        // Anything else leaves no comment, and the delivery still counts.
        for v in [json!(null), json!(5), json!(""), json!("a".repeat(65))] {
            let d = parse(&with("comment", v.clone())).unwrap();
            assert_eq!(d.comment, None, "{v}");
        }
        assert_eq!(parse(&full()).unwrap().comment, None);
    }

    #[test]
    fn a_missing_or_malformed_field_drops_the_message() {
        let bad = [
            with("type", json!("open")),
            without("type"),
            without("id"),
            with("id", json!("0123456789abcdef")),
            with("id", json!("0123456789abcdef0123456789abcdeg")),
            with("id", json!(format!("{ID}0"))),
            with("id", json!(7)),
            without("kind"),
            with("kind", json!("deleted")),
            with("kind", json!(1)),
            without("agent_session"),
            with("agent_session", json!("")),
            with("agent_session", json!("a".repeat(65))),
            with("agent_session", json!(null)),
            without("issue"),
            with("issue", json!("")),
            with("issue", json!("a".repeat(65))),
            without("organization"),
            with("organization", json!("")),
            with("organization", json!(["x"])),
            with("activity", json!(5)),
            with("activity", json!("a".repeat(65))),
            with("body", json!(5)),
            with("body", json!({"text": "hi"})),
            with("reason", json!(5)),
            json!("linear"),
            Value::Null,
        ];
        for msg in bad {
            assert!(parse(&msg).is_none(), "{msg}");
        }
        // The longest of each is still fine.
        assert!(parse(&with("issue", json!("é".repeat(64)))).is_some());
    }

    #[test]
    fn an_oversized_body_is_dropped_but_the_delivery_kept() {
        let longest = "é".repeat(16_000);
        assert_eq!(
            parse(&with("body", json!(longest))).unwrap().body,
            Some(longest)
        );
        let d = parse(&with("body", json!("x".repeat(16_001)))).unwrap();
        assert_eq!(d.body, None);
        assert_eq!(d.id, ID);
    }

    #[test]
    fn debug_shows_no_body_text() {
        let d = parse(&full()).unwrap();
        let shown = format!("{d:?}");
        assert!(!shown.contains("Please also fix"), "{shown}");
        assert!(shown.contains("body: Some("), "{shown}");
        assert!(shown.contains(ID), "{shown}");
        let none = parse(&without("body")).unwrap();
        assert!(format!("{none:?}").contains("body: None"));
    }

    #[test]
    fn guidance_keeps_each_rule_and_its_team() {
        let msg = with(
            "guidance",
            json!([
                {"team": null, "body": "Add a changelog entry."},
                {"team": "Eng / Web", "body": "Branch from develop."},
            ]),
        );
        assert_eq!(
            parse(&msg).unwrap().guidance,
            vec![
                GuidanceRule {
                    team: None,
                    body: "Add a changelog entry.".into()
                },
                GuidanceRule {
                    team: Some("Eng / Web".into()),
                    body: "Branch from develop.".into()
                },
            ]
        );
    }

    #[test]
    fn broken_or_long_guidance_is_dropped_but_the_delivery_kept() {
        let longest = "é".repeat(MAX_GUIDANCE);
        let fits = with("guidance", json!([{"team": null, "body": longest}]));
        assert_eq!(parse(&fits).unwrap().guidance.len(), 1);
        for g in [
            Value::Null,
            json!("Branch from develop."),
            json!({"body": "x"}),
            json!([{"team": null}]),
            json!([{"team": 5, "body": "x"}]),
            json!([{"team": null, "body": "ok"}, "x"]),
            json!([{"team": "T", "body": "x".repeat(MAX_GUIDANCE)}]),
        ] {
            let d = parse(&with("guidance", g.clone())).unwrap_or_else(|| panic!("{g}"));
            assert!(d.guidance.is_empty(), "{g}");
        }
        assert!(parse(&without("guidance")).unwrap().guidance.is_empty());
    }

    #[test]
    fn debug_shows_no_guidance_text() {
        let msg = with(
            "guidance",
            json!([{"team": "Secret team", "body": "Secret rule"}]),
        );
        let shown = format!("{:?}", parse(&msg).unwrap());
        assert!(!shown.contains("Secret"), "{shown}");
        assert!(shown.contains("guidance: 1"), "{shown}");
    }
}
