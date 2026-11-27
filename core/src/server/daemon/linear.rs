//! The server's Linear settings, as the desktop app sends them, and the
//! routes they make: which Linear teams and projects Arakun's server sends to
//! this server's relay label.
//!
//! `linear.json` in the server's folder holds the settings with each mapped
//! workspace's Linear key, readable by this user alone, and the workspaces
//! routes were sent for. The key is never logged or sent anywhere but
//! Linear. Nothing goes to Arakun's server without a sign-in and a relay
//! label; a failed sync waits for the next change, sign-in or relay connect.

use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::Daemon;
use crate::cloud;
use crate::linear::delegation::Routes;
use crate::linear::remote::{ServerSettings, route_plan};
use crate::phone::ClientId;

/// The file in the server's folder.
pub const FILE: &str = "linear.json";

/// What `linear.json` holds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct Kept {
    #[serde(default)]
    pub settings: ServerSettings,
    /// Linear organization ids this server sent non-empty routes for, so
    /// they are cleared once no mapping names them.
    #[serde(default)]
    pub announced: Vec<String>,
}

pub(super) fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The settings kept in `dir`. A file that cannot be read is set aside and
/// reads as off: a bad file never routes work here.
pub(super) fn load(dir: &Path) -> Kept {
    crate::stored::load(&path(dir)).unwrap_or_default()
}

fn save(dir: &Path, kept: &Kept) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(kept).map_err(|e| e.to_string())?;
    crate::durable::write_private(&path(dir), &bytes)
        .map_err(|e| format!("could not save the Linear settings: {e}"))
}

/// Where the server stands with Arakun's server about Linear.
#[derive(Default)]
pub(super) struct Linear {
    pub kept: Kept,
    /// A route sync is running.
    syncing: bool,
    /// A change came while it ran, or it failed: one more run is owed.
    owed: bool,
}

impl Linear {
    pub fn new(kept: Kept) -> Linear {
        Linear {
            kept,
            ..Linear::default()
        }
    }
}

/// What one route call did.
#[derive(Debug, PartialEq)]
enum Sent {
    /// Taken: the workspace is announced when it got routes, else cleared.
    Done {
        workspace: String,
        any: bool,
    },
    /// Nothing to clear there: no link or no machine row.
    Gone {
        workspace: String,
    },
    Failed(cloud::Error),
}

/// Sends each planned route. It blocks, so it runs off the UI thread. A
/// limit, a sign-out or no network stops it: the next one would fail too.
fn send(base: &str, session: &str, label: &str, plan: Vec<(String, Routes)>) -> Vec<Sent> {
    let mut out = Vec::new();
    for (workspace, routes) in plan {
        let any = !routes.teams.is_empty() || !routes.projects.is_empty();
        let sent = cloud::linear_put_routes(
            base,
            session,
            label,
            &workspace,
            &routes.teams,
            &routes.projects,
        );
        match sent {
            Ok(()) => out.push(Sent::Done { workspace, any }),
            // A workspace the account never linked (409) or a server the
            // account does not list (404) has nothing to clear.
            Err(cloud::Error::Status {
                status: 404 | 409, ..
            }) if !any => out.push(Sent::Gone { workspace }),
            Err(e @ cloud::Error::Status { status, .. }) if status != 429 => {
                out.push(Sent::Failed(e))
            }
            Err(e) => {
                out.push(Sent::Failed(e));
                break;
            }
        }
    }
    out
}

/// The announced workspaces after `sent`.
fn announced_after(mut announced: Vec<String>, sent: &[Sent]) -> Vec<String> {
    for s in sent {
        match s {
            Sent::Done { workspace, any } => {
                announced.retain(|w| w != workspace);
                if *any {
                    announced.push(workspace.clone());
                }
            }
            Sent::Gone { workspace } => announced.retain(|w| w != workspace),
            Sent::Failed(_) => {}
        }
    }
    announced.sort();
    announced
}

/// The answer to the app: how many mappings this server took, or why not.
pub(super) fn answer(saved: Result<usize, String>) -> String {
    match saved {
        Ok(mapped) => json!({ "type": "linear_settings", "mapped": mapped }),
        Err(e) => json!({ "type": "linear_settings", "error": e }),
    }
    .to_string()
}

impl Daemon {
    /// The places on this server, which a mapping must name.
    pub(super) fn linear_places(&self) -> Vec<PathBuf> {
        self.store.places.iter().map(|p| p.path.clone()).collect()
    }

    /// The app sent this server its Linear settings: they replace the kept
    /// ones, and Arakun's server hears the routes.
    pub(super) fn linear_settings(
        &mut self,
        client: ClientId,
        settings: ServerSettings,
        cx: &mut Context<Self>,
    ) {
        let settings = settings.tidy();
        let places = self.linear_places();
        let mapped = settings
            .mappings
            .iter()
            .filter(|m| places.contains(&m.project))
            .count();
        let mut kept = self.linear.kept.clone();
        kept.settings = settings;
        let saved = save(&super::super::dir(), &kept).map(|()| mapped);
        if saved.is_ok() {
            eprintln!(
                "arakun server: Linear: {} for {mapped} mapping(s)",
                if kept.settings.enabled { "on" } else { "off" }
            );
            self.linear.kept = kept;
        }
        self.transport.send(client, answer(saved));
        self.linear_sync(cx);
        // More slots may have been sent: runs that wait start now.
        self.linear_start_waiting(cx);
    }

    /// Brings Arakun's server in line with the kept settings and the places
    /// here. A call while one runs is owed, and makes one more run after it.
    pub(super) fn linear_sync(&mut self, cx: &mut Context<Self>) {
        let plan = route_plan(
            &self.linear.kept.settings,
            &self.linear.kept.announced,
            &self.linear_places(),
        );
        if plan.is_empty() {
            return;
        }
        // Without a usable sign-in or a relay label there is nobody to tell:
        // a sign-in or the relay's connect tries again.
        let Some(sign_in) = self.signed_in.clone().filter(|_| !self.session_dead) else {
            return;
        };
        let Some(label) = self.settings.relay_label() else {
            return;
        };
        self.linear.owed = true;
        if self.linear.syncing {
            return;
        }
        self.linear.syncing = true;
        self.linear.owed = false;
        cx.spawn(async move |this, cx| {
            let sent = cx
                .background_spawn(crate::guard::task_or(
                    async move { send(&sign_in.base, sign_in.session.expose(), &label, plan) },
                    |_| vec![Sent::Failed(cloud::Error::Unreachable("it broke".into()))],
                ))
                .await;
            let _ = this.update(cx, |this, cx| this.linear_synced(sent, cx));
        })
        .detach();
    }

    fn linear_synced(&mut self, sent: Vec<Sent>, cx: &mut Context<Self>) {
        self.linear.syncing = false;
        let announced = announced_after(self.linear.kept.announced.clone(), &sent);
        if announced != self.linear.kept.announced {
            let mut kept = self.linear.kept.clone();
            kept.announced = announced;
            match save(&super::super::dir(), &kept) {
                Ok(()) => self.linear.kept = kept,
                Err(e) => eprintln!("arakun server: {e}"),
            }
        }
        let failed = sent.iter().find_map(|s| match s {
            Sent::Failed(e) => Some(e),
            _ => None,
        });
        match failed {
            // Owed until the next change, sign-in or relay connect: no timer.
            Some(e) => {
                eprintln!("arakun server: Linear: could not send the routes: {e}");
                self.linear.owed = false;
            }
            None if std::mem::take(&mut self.linear.owed) => self.linear_sync(cx),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(w: &str, any: bool) -> Sent {
        Sent::Done {
            workspace: w.into(),
            any,
        }
    }

    #[test]
    fn a_workspace_is_announced_while_it_has_routes() {
        let after = announced_after(
            vec!["w0".into(), "w1".into()],
            &[
                done("w0", false),
                done("w2", true),
                Sent::Gone {
                    workspace: "w1".into(),
                },
            ],
        );
        assert_eq!(after, vec!["w2".to_string()]);
    }

    #[test]
    fn a_failed_call_keeps_what_was_announced() {
        let after = announced_after(
            vec!["w1".into()],
            &[Sent::Failed(cloud::Error::Unreachable("offline".into()))],
        );
        assert_eq!(after, vec!["w1".to_string()]);
    }

    #[test]
    fn the_app_hears_how_many_mappings_were_taken_or_why_not() {
        let ok: serde_json::Value = serde_json::from_str(&answer(Ok(2))).unwrap();
        assert_eq!(ok, json!({ "type": "linear_settings", "mapped": 2 }));
        let no: serde_json::Value = serde_json::from_str(&answer(Err("full disk".into()))).unwrap();
        assert_eq!(no["error"], "full disk");
    }

    #[test]
    fn the_file_round_trips_and_is_private() {
        let dir = std::env::temp_dir().join(format!("arakun-server-linear-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(load(&dir), Kept::default());
        let kept = Kept {
            settings: ServerSettings {
                enabled: true,
                mappings: Vec::new(),
                keys: vec![crate::linear::remote::WorkspaceKey {
                    workspace: "w1".into(),
                    key: "lin_1".into(),
                }],
                ..Default::default()
            },
            announced: vec!["w1".into()],
        };
        save(&dir, &kept).unwrap();
        assert_eq!(load(&dir), kept);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(path(&dir)).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_file_reads_as_off() {
        let dir =
            std::env::temp_dir().join(format!("arakun-server-linear-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(path(&dir), "{ torn").unwrap();
        assert_eq!(load(&dir), Kept::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
