//! Sending each paired server its part of the Linear opt-in: the switch, the
//! mappings to its projects, and the Linear key of each mapped workspace the
//! person agreed to send. The server keeps them and tells Arakun's server
//! which teams and projects it takes (`core/src/server/daemon/linear.rs`).
//!
//! A server goes when its mappings change or it connects. One that had
//! mappings and has none now is sent empty settings once, so it forgets the
//! key; `app_dir/linear/servers.json` remembers which servers hold some.
//! The key goes over the server link, which is end-to-end encrypted.

use std::path::PathBuf;

use gpui::{AppContext as _, Context};
use serde_json::Value;

use super::Shell;
use crate::linear::delegation::Delegation;
use crate::linear::remote::{ServerSettings, WorkspaceKey};
use crate::linear::{self, Session};
use crate::servers_view::Reach;

fn told_file() -> PathBuf {
    crate::paths::shared("linear").join("servers.json")
}

/// The servers that were sent mappings and not cleared since.
fn told() -> Vec<String> {
    crate::stored::load(&told_file()).unwrap_or_default()
}

fn set_told(server: &str, holds: bool) {
    let mut list = told();
    let before = list.clone();
    list.retain(|s| s != server);
    if holds {
        list.push(server.to_string());
    }
    if list == before {
        return;
    }
    let saved = serde_json::to_vec(&list)
        .map_err(std::io::Error::other)
        .and_then(|bytes| crate::stored::write(&told_file(), &bytes));
    if let Err(e) = saved {
        eprintln!("arakun: could not save which servers take Linear's work: {e}");
    }
}

/// The servers to send to: every one with a mapping, and every one told
/// before, which may need clearing.
pub fn recipients(d: &Delegation, told: &[String]) -> Vec<String> {
    let mut out = d.servers();
    out.extend(told.iter().cloned());
    out.sort();
    out.dedup();
    out
}

/// The workspaces whose key goes to `server`: mapped there, with the switch
/// on, and agreed to by the person.
pub fn key_workspaces(d: &Delegation, server: &str) -> Vec<String> {
    if !d.enabled {
        return Vec::new();
    }
    let mut out: Vec<String> = d
        .mappings
        .iter()
        .filter(|m| m.server.as_deref() == Some(server) && d.consents(&m.workspace))
        .map(|m| m.workspace.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The message for `server`. Never logged: it holds the keys.
pub fn message(d: &Delegation, server: &str, keys: Vec<WorkspaceKey>) -> Value {
    let mine = d.on(Some(server));
    let settings = ServerSettings {
        enabled: mine.enabled,
        mappings: mine.mappings,
        keys,
    };
    let mut msg = serde_json::to_value(settings).unwrap_or_default();
    msg["type"] = "linear_settings".into();
    msg
}

impl Shell {
    /// Sends every server that needs it its Linear settings.
    pub(super) fn linear_push_servers(&mut self, cx: &mut Context<Self>) {
        let d = Delegation::load();
        for server in recipients(&d, &told()) {
            self.linear_push_server(&server, cx);
        }
    }

    /// Sends `server` its Linear settings, when it is connected and has or
    /// had mappings. The keys are read off the UI thread: the keyring may
    /// wait for the person to unlock it.
    pub(super) fn linear_push_server(&mut self, server: &str, cx: &mut Context<Self>) {
        let d = Delegation::load();
        let has = d
            .mappings
            .iter()
            .any(|m| m.server.as_deref() == Some(server));
        if !has && !told().iter().any(|s| s == server) {
            return;
        }
        if self.server_reach(server) != Some(Reach::Connected) {
            return;
        }
        let sessions = linear::sessions();
        let sign_ins: Vec<(String, Session)> = key_workspaces(&d, server)
            .into_iter()
            .filter_map(|w| Some((w.clone(), linear::session_for(&sessions, &w)?.clone())))
            .collect();
        let server = server.to_string();
        cx.spawn(async move |this, cx| {
            let keys = cx
                .background_spawn(crate::guard::task_or(
                    async move {
                        sign_ins
                            .into_iter()
                            .filter_map(|(workspace, s)| {
                                let key = linear::key_for(&s).ok()?;
                                Some(WorkspaceKey {
                                    workspace,
                                    key: key.into(),
                                })
                            })
                            .collect::<Vec<_>>()
                    },
                    |_| Vec::new(),
                ))
                .await;
            let _ = this.update(cx, |this, _| {
                // The settings may have changed while the keyring was read:
                // the mappings sent are the ones saved now.
                let d = Delegation::load();
                let wanted = key_workspaces(&d, &server);
                let keys = keys
                    .into_iter()
                    .filter(|k| wanted.contains(&k.workspace))
                    .collect();
                this.server_send_raw(&server, &message(&d, &server, keys));
                let holds = d
                    .mappings
                    .iter()
                    .any(|m| m.server.as_deref() == Some(&*server));
                set_told(&server, holds);
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::delegation::{Kind, Mapping};

    fn mapping(workspace: &str, server: Option<&str>) -> Mapping {
        Mapping {
            workspace: workspace.into(),
            kind: Kind::Team,
            target: format!("t-{workspace}"),
            name: "Eng".into(),
            project: PathBuf::from("/srv/app"),
            server: server.map(str::to_string),
        }
    }

    fn delegation(enabled: bool, mappings: Vec<Mapping>, consented: &[&str]) -> Delegation {
        Delegation {
            enabled,
            mappings,
            consented: consented.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn servers_with_mappings_and_those_told_before_are_sent_to() {
        let d = delegation(
            true,
            vec![mapping("w1", Some("s1")), mapping("w1", None)],
            &[],
        );
        assert_eq!(recipients(&d, &["s2".into(), "s1".into()]), ["s1", "s2"]);
    }

    #[test]
    fn a_key_goes_only_on_with_consent_and_only_to_its_server() {
        let d = delegation(
            true,
            vec![
                mapping("w1", Some("s1")),
                mapping("w2", Some("s1")),
                mapping("w3", Some("s2")),
            ],
            &["w1", "w3"],
        );
        assert_eq!(key_workspaces(&d, "s1"), ["w1"]);
        assert_eq!(key_workspaces(&d, "s2"), ["w3"]);
        let off = delegation(false, d.mappings.clone(), &["w1", "w3"]);
        assert!(key_workspaces(&off, "s1").is_empty());
    }

    #[test]
    fn a_server_gets_only_its_own_mappings_without_the_server_named() {
        let d = delegation(
            true,
            vec![
                mapping("w1", Some("s1")),
                mapping("w2", Some("s2")),
                mapping("w3", None),
            ],
            &[],
        );
        let key = WorkspaceKey {
            workspace: "w1".into(),
            key: "lin_1".into(),
        };
        let msg = message(&d, "s1", vec![key]);
        assert_eq!(msg["type"], "linear_settings");
        assert_eq!(msg["enabled"], true);
        let mappings = msg["mappings"].as_array().unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0]["workspace"], "w1");
        assert!(mappings[0].get("server").is_none());
        // What the server reads it as.
        let back: crate::phone::ClientMsg = serde_json::from_value(msg).unwrap();
        let crate::phone::ClientMsg::LinearSettings(s) = back else {
            panic!("not linear_settings");
        };
        assert_eq!(s.key_for("w1"), Some("lin_1"));
    }

    #[test]
    fn a_server_with_no_mappings_left_is_sent_empty_settings() {
        let d = delegation(true, vec![mapping("w1", None)], &["w1"]);
        let msg = message(&d, "s1", Vec::new());
        assert_eq!(msg["mappings"], serde_json::json!([]));
        assert_eq!(msg["keys"], serde_json::json!([]));
    }
}
