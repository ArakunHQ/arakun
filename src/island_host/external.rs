//! Agents that run outside Arakun (Claude Code in a terminal or an editor) on
//! the island, fed by their hooks through `arakun island hook`, or by hand with
//! `arakun island session`. Windows only for now.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::Path;

use gpui::SharedString;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::proto::ToIsland;
use crate::island::{self as isl, Alert, AlertKind, Feed, Row};

/// Finished and failed sessions stay in the list this long, as the app's do.
const RECENT_SECS: u64 = 10 * 60;
/// A session silent this long is gone: its terminal closed mid-turn.
const STALE_SECS: u64 = 2 * 60 * 60;
/// Keys of outside sessions start with this, so the app's and the headless
/// ones never collide with them and "Ver" knows there is nothing to open.
pub const PREFIX: &str = "ext:";
/// Who an update comes from when the hook doesn't say.
const AGENT: &str = "Claude Code";

/// What an outside session is up to.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Working,
    Waiting,
    Done,
    Error,
    /// The session closed: it leaves the list.
    Ended,
}

impl State {
    pub fn from_name(name: &str) -> Option<State> {
        Some(match name {
            "working" => State::Working,
            "waiting" => State::Waiting,
            "done" => State::Done,
            "error" => State::Error,
            "ended" => State::Ended,
            _ => return None,
        })
    }
}

/// One change of an outside session, as `ToIsland::Session` carries it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Update {
    /// The session, `PREFIX` included.
    pub key: SharedString,
    pub state: State,
    #[serde(default)]
    pub agent: SharedString,
    #[serde(default)]
    pub project: SharedString,
    /// The prompt it works on, or what it waits for; empty keeps the last one.
    #[serde(default)]
    pub doing: SharedString,
}

struct Session {
    state: State,
    agent: SharedString,
    project: SharedString,
    /// The prompt it works on, for its row and the card when it ends.
    prompt: SharedString,
    /// What it waits for while it waits.
    asks: SharedString,
    updated: u64,
}

impl Session {
    fn about(&self, what: &SharedString) -> SharedString {
        match (self.project.is_empty(), what.is_empty()) {
            (true, _) => what.clone(),
            (false, true) => self.project.clone(),
            (false, false) => format!("{} · {what}", self.project).into(),
        }
    }
}

/// The outside sessions the island knows.
#[derive(Default)]
pub struct Sessions {
    all: HashMap<SharedString, Session>,
}

impl Sessions {
    /// Takes an update; the card to show when a turn ends or waits for the user.
    pub fn update(&mut self, update: Update, now: u64) -> Option<Alert> {
        if update.state == State::Ended {
            self.all.remove(&update.key);
            return None;
        }
        let s = self.all.entry(update.key.clone()).or_insert(Session {
            state: State::Ended,
            agent: SharedString::default(),
            project: SharedString::default(),
            prompt: SharedString::default(),
            asks: SharedString::default(),
            updated: now,
        });
        // Claude says it waits for input a while after a turn ended: not news.
        if update.state == State::Waiting && s.state == State::Done {
            return None;
        }
        let before = s.state;
        s.state = update.state;
        s.updated = now;
        if !update.agent.is_empty() {
            s.agent = update.agent;
        }
        if !update.project.is_empty() {
            s.project = update.project;
        }
        match update.state {
            State::Working if !update.doing.is_empty() => s.prompt = update.doing,
            State::Waiting | State::Error => s.asks = update.doing,
            _ => {}
        }
        if before == update.state {
            return None;
        }
        let (kind, title, about) = match update.state {
            State::Waiting => (AlertKind::Input, "te necesita", &s.asks),
            State::Done => (AlertKind::Finished, "terminó", &s.prompt),
            State::Error => (AlertKind::Error, "falló", &s.asks),
            State::Working | State::Ended => return None,
        };
        Some(Alert {
            key: update.key,
            kind,
            title: format!("{} {title}", s.agent).into(),
            subtitle: s.about(about),
            req: SharedString::default(),
            image: SharedString::default(),
            brand: SharedString::default(),
        })
    }

    /// The rows to list, forgetting sessions done long ago or gone silent.
    pub fn rows(&mut self, now: u64) -> Vec<Row> {
        self.all.retain(|_, s| {
            let quiet = now.saturating_sub(s.updated);
            let settled = matches!(s.state, State::Done | State::Error);
            quiet < STALE_SECS && !(settled && quiet >= RECENT_SECS)
        });
        self.all
            .iter()
            .map(|(key, s)| Row {
                key: key.clone(),
                status: match s.state {
                    State::Working | State::Ended => isl::Status::Working,
                    State::Waiting => isl::Status::Waiting,
                    State::Done => isl::Status::Done,
                    State::Error => isl::Status::Error,
                },
                model: s.agent.clone(),
                doing: s.about(&s.prompt),
                elapsed: SharedString::default(),
                cost: SharedString::default(),
                updated: s.updated,
            })
            .collect()
    }
}

/// The app's and the headless agents' feed with the outside sessions in it.
pub fn merge(mut feed: Feed, sessions: &mut Sessions) -> Feed {
    feed.rows.extend(sessions.rows(crate::model::unix_now()));
    isl::sort_rows(&mut feed.rows);
    feed
}

/// The update a Claude Code hook's input stands for, `None` for an event the
/// island has nothing to show for.
pub fn from_hook(input: &Value, agent: &str) -> Option<Update> {
    let id = input["session_id"].as_str().filter(|id| !id.is_empty())?;
    let text = |field: &str| isl::one_line(input[field].as_str().unwrap_or_default());
    let (state, doing) = match input["hook_event_name"].as_str()? {
        "UserPromptSubmit" => (State::Working, text("prompt")),
        // Back to work after a permission prompt was answered.
        "PostToolUse" => (State::Working, String::new()),
        "Notification" => (State::Waiting, text("message")),
        "Stop" => (State::Done, String::new()),
        "SessionEnd" => (State::Ended, String::new()),
        _ => return None,
    };
    Some(Update {
        key: format!("{PREFIX}{id}").into(),
        state,
        agent: agent.to_string().into(),
        project: project_of(input["cwd"].as_str().unwrap_or_default()),
        doing: doing.into(),
    })
}

/// `arakun island session <id> <state> [project] [doing]`.
fn from_args(args: &[String]) -> Option<Update> {
    let [id, state, rest @ ..] = args else {
        return None;
    };
    if rest.len() > 2 || id.is_empty() {
        return None;
    }
    Some(Update {
        key: format!("{PREFIX}{id}").into(),
        state: State::from_name(state)?,
        agent: "Agente".into(),
        project: rest.first().cloned().unwrap_or_default().into(),
        doing: rest
            .get(1)
            .map(|d| isl::one_line(d))
            .unwrap_or_default()
            .into(),
    })
}

/// `arakun island hook` and `arakun island session`; `None` for other requests.
pub fn cli(args: &[String]) -> Option<i32> {
    match args.first().map(String::as_str) {
        Some("hook") => {
            // A hook must never get in the agent's way: whatever happens, it says nothing.
            let mut input = String::new();
            let _ = std::io::stdin().read_to_string(&mut input);
            let agent = args.get(1).map(String::as_str).unwrap_or(AGENT);
            let update = serde_json::from_str(&input)
                .ok()
                .and_then(|v| from_hook(&v, agent));
            if let Some(update) = update {
                let _ = super::proto::request(&ToIsland::Session { update });
            }
            Some(0)
        }
        Some("session") => Some(match from_args(&args[1..]) {
            Some(update) => super::send(ToIsland::Session { update }),
            None => {
                eprintln!("usage: {}", crate::control::ISLAND_USAGE);
                2
            }
        }),
        _ => None,
    }
}

fn project_of(cwd: &str) -> SharedString {
    Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1_800_000_000;

    fn update(id: &str, state: State, doing: &str) -> Update {
        Update {
            key: format!("{PREFIX}{id}").into(),
            state,
            agent: "Claude Code".into(),
            project: "ArakunHQ".into(),
            doing: doing.into(),
        }
    }

    #[test]
    fn reads_a_state_by_name() {
        assert_eq!(State::from_name("working"), Some(State::Working));
        assert_eq!(State::from_name("waiting"), Some(State::Waiting));
        assert_eq!(State::from_name("done"), Some(State::Done));
        assert_eq!(State::from_name("error"), Some(State::Error));
        assert_eq!(State::from_name("ended"), Some(State::Ended));
        assert_eq!(State::from_name("dancing"), None);
    }

    #[test]
    fn reads_claude_code_hooks() {
        let hook = |event: &str, extra: Value| {
            let mut v = json!({
                "session_id": "abc",
                "cwd": "C:\\Users\\me\\proyectos\\ArakunHQ",
                "hook_event_name": event,
            });
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            from_hook(&v, "Claude Code")
        };
        let prompt = hook("UserPromptSubmit", json!({"prompt": "arregla el\nbuild"})).unwrap();
        assert_eq!(prompt.key, "ext:abc");
        assert_eq!(prompt.state, State::Working);
        assert_eq!(prompt.project, "ArakunHQ");
        assert_eq!(prompt.agent, "Claude Code");
        // Its first line, as the island shows every text.
        assert_eq!(prompt.doing, "arregla el");
        let tool = hook("PostToolUse", json!({"tool_name": "Bash"})).unwrap();
        assert_eq!((tool.state, tool.doing.as_ref()), (State::Working, ""));
        let asks = hook(
            "Notification",
            json!({"message": "Claude needs your permission to use Bash"}),
        )
        .unwrap();
        assert_eq!(asks.state, State::Waiting);
        assert_eq!(asks.doing, "Claude needs your permission to use Bash");
        assert_eq!(hook("Stop", json!({})).unwrap().state, State::Done);
        assert_eq!(hook("SessionEnd", json!({})).unwrap().state, State::Ended);
        assert_eq!(hook("PreCompact", json!({})), None);
        assert_eq!(from_hook(&json!({"hook_event_name": "Stop"}), "x"), None);
    }

    #[test]
    fn takes_a_session_by_hand() {
        let args = |v: &[&str]| from_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let u = args(&["s1", "waiting", "web", "¿Despliego?"]).unwrap();
        assert_eq!(u.key, "ext:s1");
        assert_eq!(u.state, State::Waiting);
        assert_eq!(
            (u.project.as_ref(), u.doing.as_ref()),
            ("web", "¿Despliego?")
        );
        assert_eq!(u.agent, "Agente");
        assert_eq!(args(&["s1", "done"]).unwrap().project, "");
        assert_eq!(args(&["s1", "dancing"]), None);
        assert_eq!(args(&["s1"]), None);
        assert_eq!(args(&["s1", "done", "p", "d", "x"]), None);
    }

    #[test]
    fn a_card_when_a_turn_ends_or_waits() {
        let mut s = Sessions::default();
        assert_eq!(
            s.update(update("a", State::Working, "arregla el build"), NOW),
            None
        );
        assert_eq!(s.update(update("a", State::Working, ""), NOW + 5), None);
        let asks = s
            .update(
                update("a", State::Waiting, "needs your permission"),
                NOW + 10,
            )
            .unwrap();
        assert_eq!(asks.kind, AlertKind::Input);
        assert_eq!(asks.key, "ext:a");
        assert_eq!(asks.title, "Claude Code te necesita");
        assert_eq!(asks.subtitle, "ArakunHQ · needs your permission");
        assert_eq!(s.update(update("a", State::Working, ""), NOW + 20), None);
        let done = s.update(update("a", State::Done, ""), NOW + 30).unwrap();
        assert_eq!(done.kind, AlertKind::Finished);
        assert_eq!(done.title, "Claude Code terminó");
        // The prompt it worked on, not what it last waited for.
        assert_eq!(done.subtitle, "ArakunHQ · arregla el build");
        // A second Stop, or "waiting for your input" once done, is no news.
        assert_eq!(s.update(update("a", State::Done, ""), NOW + 40), None);
        assert_eq!(
            s.update(
                update("a", State::Waiting, "waiting for your input"),
                NOW + 99
            ),
            None
        );
        let failed = s.update(update("b", State::Error, "sin red"), NOW).unwrap();
        assert_eq!(failed.kind, AlertKind::Error);
        assert_eq!(failed.title, "Claude Code falló");
    }

    #[test]
    fn lists_sessions_until_they_end_or_go_stale() {
        let mut s = Sessions::default();
        s.update(update("busy", State::Working, "migra la base"), NOW);
        s.update(update("asks", State::Working, "despliega"), NOW);
        s.update(update("asks", State::Waiting, ""), NOW + 1);
        s.update(update("done", State::Done, ""), NOW);
        s.update(update("gone", State::Working, ""), NOW);
        s.update(update("gone", State::Ended, ""), NOW + 1);
        let rows = s.rows(NOW + 2);
        let row = |k: &str| rows.iter().find(|r| r.key == format!("{PREFIX}{k}"));
        assert_eq!(rows.len(), 3);
        assert!(row("gone").is_none());
        let busy = row("busy").unwrap();
        assert_eq!(busy.status, isl::Status::Working);
        assert_eq!(busy.model, "Claude Code");
        assert_eq!(busy.doing, "ArakunHQ · migra la base");
        assert_eq!(busy.updated, NOW);
        assert_eq!(row("asks").unwrap().status, isl::Status::Waiting);
        assert_eq!(row("asks").unwrap().doing, "ArakunHQ · despliega");
        assert_eq!(row("done").unwrap().status, isl::Status::Done);
        // Done ones go after a while; any that went silent for long, too.
        assert_eq!(s.rows(NOW + RECENT_SECS + 1).len(), 2);
        assert!(s.rows(NOW + STALE_SECS + 2).is_empty());
    }
}
