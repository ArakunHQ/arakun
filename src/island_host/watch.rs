//! Headless agents (`crate::headless`) on the island, read straight from their
//! folders so they show with the app closed: a row while busy or lately done,
//! a card when one finishes or fails, and one to approve or reject each tool
//! call waiting in a `pending.json`.

use std::collections::{HashMap, HashSet};

use gpui::SharedString;

use super::brief;
use crate::agent::Entry;
use crate::headless::{self, Meta, Pending, Status};
use crate::island::{self as isl, Alert, AlertKind, Feed, Row};

/// Finished and failed sessions stay in the list this long, as the app's do.
const RECENT_SECS: u64 = 10 * 60;

/// The headless folder at one moment.
pub struct Scan {
    pub now: u64,
    /// Each session, its status as a reader takes it, and its unanswered call.
    pub sessions: Vec<(Meta, Status, Option<Pending>)>,
}

pub fn scan() -> Scan {
    let now = crate::model::unix_now();
    let sessions = headless::list()
        .into_iter()
        .map(|m| {
            let status = headless::status(&m);
            let pending = (status == Status::NeedsInput)
                .then(|| headless::read_pending(&m.id))
                .flatten()
                .filter(|p| p.answer.is_none() && p.expires > now);
            (m, status, pending)
        })
        .collect();
    Scan { now, sessions }
}

/// What the island shows of the headless agents.
#[derive(Default, Debug, PartialEq)]
pub struct Seen {
    pub rows: Vec<Row>,
    pub today: Option<SharedString>,
    /// Cards that came up since the last scan.
    pub alerts: Vec<Alert>,
    /// Session and request of every call still waiting.
    pub pending: HashSet<(String, String)>,
}

/// Remembers the last scan, to tell what changed since.
#[derive(Default)]
pub struct Watch {
    last: HashMap<String, Status>,
    asked: HashSet<(String, String)>,
    started: bool,
}

impl Watch {
    pub fn update(&mut self, scan: &Scan) -> Seen {
        let mut seen = Seen::default();
        let today = crate::platform::local_time(scan.now).map(|t| (t.year, t.month, t.day));
        let mut spent = 0.;
        for (m, status, pending) in &scan.sessions {
            let model = model_label(m);
            let recent = scan.now.saturating_sub(m.updated) < RECENT_SECS;
            let row = match status {
                Status::Working => Some(isl::Status::Working),
                Status::NeedsInput => Some(isl::Status::Waiting),
                Status::Error if recent => Some(isl::Status::Error),
                Status::Idle if recent => Some(isl::Status::Done),
                _ => None,
            };
            if let Some(status) = row {
                seen.rows.push(Row {
                    key: m.id.clone().into(),
                    status,
                    model: model.clone().into(),
                    doing: doing(m).into(),
                    elapsed: SharedString::default(),
                    cost: if m.cost > 0. {
                        crate::agent::fmt_cost(m.cost, "USD", false).into()
                    } else {
                        SharedString::default()
                    },
                    updated: m.updated,
                });
            }
            if crate::platform::local_time(m.updated).map(|t| (t.year, t.month, t.day)) == today {
                spent += m.cost;
            }
            // A turn that ended since the last look; one that came and went
            // between two looks counts too, once the island was up.
            let before = self.last.insert(m.id.clone(), *status);
            let ended = match before {
                Some(b) => b != *status && matches!(b, Status::Working | Status::NeedsInput),
                None => self.started && scan.now.saturating_sub(m.updated) < 10,
            };
            let card = |kind, title: String, subtitle: String| Alert {
                key: m.id.clone().into(),
                kind,
                title: title.into(),
                subtitle: subtitle.into(),
                req: SharedString::default(),
                image: SharedString::default(),
                brand: SharedString::default(),
            };
            match status {
                Status::Idle if ended => seen.alerts.push(card(
                    AlertKind::Finished,
                    format!("{model} finished"),
                    told(m),
                )),
                Status::Error if ended => {
                    let why = m
                        .error
                        .as_deref()
                        .map(brief)
                        .filter(|l| !l.is_empty())
                        .unwrap_or_else(|| told(m));
                    seen.alerts
                        .push(card(AlertKind::Error, format!("{model} failed"), why));
                }
                _ => {}
            }
            if let Some(p) = pending {
                let ask = (m.id.clone(), p.req_id.clone());
                if self.asked.insert(ask.clone()) {
                    seen.alerts.push(Alert {
                        key: m.id.clone().into(),
                        kind: AlertKind::Confirm,
                        title: isl::one_line(&p.summary).into(),
                        subtitle: format!("{model} · {}", brief(&p.why)).into(),
                        req: p.req_id.clone().into(),
                        image: SharedString::default(),
                        brand: SharedString::default(),
                    });
                }
                seen.pending.insert(ask);
            }
        }
        self.asked.retain(|a| seen.pending.contains(a));
        self.last
            .retain(|id, _| scan.sessions.iter().any(|(m, _, _)| &m.id == id));
        self.started = true;
        isl::sort_rows(&mut seen.rows);
        seen.today = (spent > 0.).then(|| crate::agent::fmt_cost(spent, "USD", false).into());
        seen
    }
}

/// "GLM 5.3" for `workers-ai/@cf/zai-org/glm-5.3`; the id's last part for another.
fn model_label(m: &Meta) -> String {
    let id = m
        .model
        .strip_prefix(crate::agent::workers_ai::CLI_PREFIX)
        .unwrap_or(&m.model);
    match crate::agent::workers_ai::model(id) {
        Some(known) => known.name.to_string(),
        None => id
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(m.agent.label())
            .to_string(),
    }
}

fn doing(m: &Meta) -> String {
    Some(isl::one_line(&m.last))
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| isl::one_line(&m.title))
}

/// What a card says until `read_replies` finds more: the last thing it did.
fn told(m: &Meta) -> String {
    Some(brief(&m.last))
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| isl::one_line(&m.title))
}

/// Gives each finished turn's card the turn's reply, from its transcript;
/// it reads files, so off the main thread.
pub fn read_replies(alerts: &mut [Alert]) {
    for alert in alerts.iter_mut().filter(|a| a.kind == AlertKind::Finished) {
        if let Some(reply) =
            headless::read_transcript(&alert.key).and_then(|t| last_reply(&t.entries))
        {
            alert.subtitle = reply.into();
        }
    }
}

/// The turn's last reply, as a card tells it.
fn last_reply(entries: &[Entry]) -> Option<String> {
    entries
        .iter()
        .rev()
        .take_while(|e| !matches!(e, Entry::User { .. }))
        .find_map(|e| match e {
            Entry::Agent { text } => Some(brief(text)).filter(|b| !b.is_empty()),
            _ => None,
        })
}

/// The app's sessions and the headless ones in one list; a headless session
/// the app lists too shows as read here, the same with the app open or not.
pub fn merge(app: Option<&Feed>, headless: &Seen) -> Feed {
    let mut rows = headless.rows.clone();
    if let Some(app) = app {
        rows.extend(
            app.rows
                .iter()
                .filter(|r| !headless.rows.iter().any(|h| h.key == r.key))
                .cloned(),
        );
    }
    isl::sort_rows(&mut rows);
    Feed {
        rows,
        today: match app {
            Some(app) => app.today.clone(),
            None => headless.today.clone(),
        },
        viewing: app.and_then(|f| f.viewing.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentKind;
    use crate::headless::policy::{Level, Policy};

    const NOW: u64 = 1_800_000_000;

    fn meta(id: &str, status: Status, updated: u64) -> Meta {
        serde_json::from_value(serde_json::json!({
            "id": id, "agent": "workers-ai", "model": "workers-ai/@cf/zai-org/glm-5.3",
            "policy": "workspace", "cwd": "/tmp/x", "title": "arregla el build", "created": 1,
            "updated": updated, "status": status, "last": "cargo test: 3 passed", "cost": 0.5,
        }))
        .unwrap()
    }

    fn pending(id: &str, req: &str) -> Pending {
        Pending {
            req_id: req.into(),
            session: id.into(),
            tool: "execute".into(),
            title: "git push".into(),
            command: Some("git push".into()),
            args: serde_json::json!({"command": "git push"}),
            level: Level::Always,
            why: "git push publica commits en el remoto".into(),
            summary: "Hacer git push de main a origin".into(),
            phrase: "confirm: git push".into(),
            created: NOW,
            expires: NOW + 60,
            answer: None,
            note: None,
        }
    }

    fn scan(sessions: Vec<(Meta, Status, Option<Pending>)>) -> Scan {
        Scan { now: NOW, sessions }
    }

    #[test]
    fn sessions_become_rows() {
        let mut w = Watch::default();
        let seen = w.update(&scan(vec![
            (meta("old", Status::Idle, NOW - 3600), Status::Idle, None),
            (meta("done", Status::Idle, NOW - 30), Status::Idle, None),
            (
                meta("busy", Status::Working, NOW - 1),
                Status::Working,
                None,
            ),
            (meta("dead", Status::Working, NOW - 5), Status::Error, None),
        ]));
        let rows: Vec<(&str, isl::Status)> = seen
            .rows
            .iter()
            .map(|r| (r.key.as_ref(), r.status))
            .collect();
        assert_eq!(
            rows,
            [
                ("dead", isl::Status::Error),
                ("busy", isl::Status::Working),
                ("done", isl::Status::Done)
            ]
        );
        assert_eq!(seen.rows[1].model.as_ref(), "GLM 5.3");
        assert_eq!(seen.rows[1].doing.as_ref(), "cargo test: 3 passed");
        assert_eq!(seen.rows[1].cost.as_ref(), "$0.50");
        // What was there when the island started is no news.
        assert!(seen.alerts.is_empty());
        let mut m = meta("x", Status::Idle, NOW);
        m.agent = AgentKind::WorkersAi;
        m.model = "workers-ai/@cf/some/new-model".into();
        assert_eq!(model_label(&m), "new-model");
        assert_eq!(meta("y", Status::Idle, NOW).policy, Policy::Workspace);
    }

    #[test]
    fn a_turn_that_ends_brings_a_card() {
        let mut w = Watch::default();
        w.update(&scan(vec![
            (meta("a", Status::Working, NOW), Status::Working, None),
            (meta("b", Status::Working, NOW), Status::Working, None),
        ]));
        let mut b = meta("b", Status::Error, NOW);
        b.error = Some("Workers AI: 429 too many requests\nretry in 20 s".into());
        let seen = w.update(&scan(vec![
            (meta("a", Status::Idle, NOW), Status::Idle, None),
            (b, Status::Error, None),
        ]));
        let cards: Vec<(AlertKind, &str, &str)> = seen
            .alerts
            .iter()
            .map(|a| (a.kind, a.title.as_ref(), a.subtitle.as_ref()))
            .collect();
        assert_eq!(
            cards,
            [
                (
                    AlertKind::Finished,
                    "GLM 5.3 finished",
                    "cargo test: 3 passed"
                ),
                (
                    AlertKind::Error,
                    "GLM 5.3 failed",
                    "Workers AI: 429 too many requests retry in 20 s"
                ),
            ]
        );
        // Told once.
        let seen = w.update(&scan(vec![(
            meta("a", Status::Idle, NOW),
            Status::Idle,
            None,
        )]));
        assert!(seen.alerts.is_empty());
        // One that started and finished between two looks.
        let seen = w.update(&scan(vec![(
            meta("c", Status::Idle, NOW - 2),
            Status::Idle,
            None,
        )]));
        assert_eq!(seen.alerts.len(), 1);
    }

    #[test]
    fn a_card_says_the_replys_opening_in_brief() {
        let reply = "## Listo\n\nArreglé el **build**: faltaba `serde` en\nCargo.toml ([ver](https://x.dev)).\n\n```rust\nfn main() {}\n```\n- 3 pruebas pasan\n1. sin warnings\n\n---\n| a | b |\n|---|---|";
        assert_eq!(
            brief(reply),
            "Listo Arreglé el build: faltaba serde en Cargo.toml (ver). 3 pruebas pasan sin warnings a b"
        );
        assert_eq!(brief("```\nonly code\n```"), "");
        assert_eq!(brief("[x] not a link, [y](u) is"), "[x] not a link, y is");
        // Long: cut near the cap, at a word's end.
        let long = "palabra ".repeat(100);
        let cut = brief(&long);
        assert!(
            cut.ends_with("palabra…") && (290..=301).contains(&cut.chars().count()),
            "{cut}"
        );
        let one_word = "x".repeat(500);
        assert_eq!(brief(&one_word).chars().count(), 301);
        // The turn's last reply, not an earlier turn's.
        let entry = |v: serde_json::Value| -> Entry { serde_json::from_value(v).unwrap() };
        let user = entry(serde_json::json!({"type": "user", "text": "go", "at": 0}));
        let agent = |t: &str| entry(serde_json::json!({"type": "agent", "text": t}));
        assert_eq!(
            last_reply(&[
                user.clone(),
                agent("**Hecho**\ncon detalle"),
                agent("```\n```")
            ])
            .as_deref(),
            Some("Hecho con detalle")
        );
        assert_eq!(last_reply(&[agent("viejo"), user]), None);
        // Until the transcript is read, the card says the last thing it did.
        let mut m = meta("a", Status::Idle, NOW);
        m.last = "**cargo test**: 3 passed".into();
        assert_eq!(told(&m), "cargo test: 3 passed");
        m.last.clear();
        assert_eq!(told(&m), "arregla el build");
    }

    #[test]
    fn a_waiting_call_asks_once_until_answered() {
        let mut w = Watch::default();
        let waiting = || {
            vec![(
                meta("h1", Status::NeedsInput, NOW),
                Status::NeedsInput,
                Some(pending("h1", "r1")),
            )]
        };
        let seen = w.update(&scan(waiting()));
        assert_eq!(seen.rows[0].status, isl::Status::Waiting);
        let [card] = &seen.alerts[..] else {
            panic!("{:?}", seen.alerts)
        };
        assert_eq!(card.kind, AlertKind::Confirm);
        assert_eq!((card.key.as_ref(), card.req.as_ref()), ("h1", "r1"));
        assert_eq!(card.title.as_ref(), "Hacer git push de main a origin");
        assert_eq!(
            card.subtitle.as_ref(),
            "GLM 5.3 · git push publica commits en el remoto"
        );
        assert!(seen.pending.contains(&("h1".to_string(), "r1".to_string())));
        assert!(w.update(&scan(waiting())).alerts.is_empty());
        // Answered: gone; the next request asks again.
        let seen = w.update(&scan(vec![(
            meta("h1", Status::Working, NOW),
            Status::Working,
            None,
        )]));
        assert!(seen.pending.is_empty() && seen.alerts.is_empty());
        let seen = w.update(&scan(vec![(
            meta("h1", Status::NeedsInput, NOW),
            Status::NeedsInput,
            Some(pending("h1", "r2")),
        )]));
        assert_eq!(seen.alerts[0].req.as_ref(), "r2");
    }

    #[test]
    fn merge_prefers_the_headless_rows_and_the_apps_total() {
        let row = |key: &str, model: &str| Row {
            key: key.to_string().into(),
            status: isl::Status::Working,
            model: model.to_string().into(),
            doing: SharedString::default(),
            elapsed: SharedString::default(),
            cost: SharedString::default(),
            updated: 0,
        };
        let headless = Seen {
            rows: vec![row("h1", "GLM 5.3")],
            today: Some("$0.50".into()),
            ..Default::default()
        };
        let app = Feed {
            rows: vec![row("h1", "Workers AI"), row("s1", "Opus 5.5")],
            today: Some("$3.00".into()),
            viewing: Some("s1".into()),
        };
        let both = merge(Some(&app), &headless);
        let rows: Vec<(&str, &str)> = both
            .rows
            .iter()
            .map(|r| (r.key.as_ref(), r.model.as_ref()))
            .collect();
        assert_eq!(rows, [("h1", "GLM 5.3"), ("s1", "Opus 5.5")]);
        assert_eq!(
            (both.today.as_deref(), both.viewing.as_deref()),
            (Some("$3.00"), Some("s1"))
        );
        let alone = merge(None, &headless);
        assert_eq!(
            (alone.rows.len(), alone.today.as_deref(), alone.viewing),
            (1, Some("$0.50"), None)
        );
        assert_eq!(merge(None, &Seen::default()), Feed::default());
    }
}
