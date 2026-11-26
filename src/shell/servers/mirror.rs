//! Mirrors what this computer installed from the marketplace onto every paired
//! server (`agent::mirror`): when a server connects, and after each install
//! here. Over plain http only the plugins and skills go, never MCP servers or
//! sign-ins, which hold keys.

use super::*;
use crate::agent::mirror;
use crate::island::AlertKind;

impl Shell {
    /// Mirrors onto every connected server.
    pub(in crate::shell) fn servers_mirror_all(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .servers
            .as_ref()
            .map(|l| {
                l.conns
                    .iter()
                    .filter(|(_, c)| c.reach == Reach::Connected)
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            self.server_mirror(&id, cx);
        }
    }

    /// Sends `server` what this computer has, read off the main thread.
    pub(super) fn server_mirror(&mut self, server: &str, cx: &mut Context<Self>) {
        let Some(link) = &self.servers else { return };
        let Some(conn) = link.conns.get(server) else {
            return;
        };
        if conn.reach != Reach::Connected {
            return;
        }
        let secrets = !link
            .saved
            .iter()
            .find(|s| s.id == server)
            .is_some_and(in_the_clear);
        let sender = conn.client.sender();
        let server = server.to_string();
        cx.spawn(async move |this, cx| {
            let too_big = cx
                .background_spawn(crate::guard::task_or_default(async move {
                    let snapshot = mirror::snapshot(secrets);
                    if snapshot.is_empty() {
                        return None;
                    }
                    let msg = snapshot.message();
                    // A message bigger than a frame would not get there.
                    if msg.to_string().len() > wire::FRAME_TEXT {
                        return Some(());
                    }
                    sender.send(&msg);
                    None
                }))
                .await;
            if too_big.is_none() {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                let machine = this.server_name_of(&server);
                this.notify(
                    AlertKind::Error,
                    &format!("Could not mirror to {machine}"),
                    "What this computer has installed is too big to send in one message. Install the rest there from the Marketplace.",
                    None,
                    cx,
                );
            });
        })
        .detach();
    }

    fn server_name_of(&self, server: &str) -> String {
        self.server_targets()
            .into_iter()
            .find(|t| t.id == server)
            .map_or_else(|| "the server".to_string(), |t| t.name)
    }

    /// A server's answer to a mirror: a notice when it added or failed something.
    pub(super) fn server_mirrored(&mut self, server: &str, msg: &Value, cx: &mut Context<Self>) {
        let names = |key: &str| -> Vec<String> {
            msg[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().or_else(|| v["name"].as_str()))
                .map(str::to_string)
                .collect()
        };
        let added = names("added");
        let failed: Vec<String> = msg["failed"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| format!("{}: {}", text(f, "name"), text(f, "error")))
            .collect();
        if added.is_empty() && failed.is_empty() {
            return;
        }
        let machine = self.server_name_of(server);
        if !added.is_empty() {
            self.views
                .extensions
                .update(cx, |v, cx| v.server_changed(server, cx));
            self.notify(
                AlertKind::Info,
                &format!("Mirrored to {machine}"),
                &format!("Added {}.", added.join(", ")),
                None,
                cx,
            );
        }
        if !failed.is_empty() {
            eprintln!("arakun: mirroring to {server}: {}", failed.join("; "));
            self.notify(
                AlertKind::Error,
                &format!("Could not mirror everything to {machine}"),
                &failed.join("\n"),
                None,
                cx,
            );
        }
    }
}
