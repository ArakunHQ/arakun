//! Discover on a paired server (`crate::servers`): the plugins its
//! marketplaces offer and the MCP servers Claude has there, with Install
//! doing its work over there. The view asks through the shell, which holds
//! the connections (`ExtensionsEvent::Server`), and hears back in
//! [`ExtensionsView::server_said`]. The MCP Registry is the same from
//! everywhere, so it is still read from here.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::*;
use crate::agent::marketplace::PluginEntry;
use crate::servers_view::Reach;

/// A paired server Discover can install on.
#[derive(Clone, PartialEq)]
pub struct ServerTarget {
    pub id: String,
    pub name: String,
    pub reach: Reach,
    /// Reached over plain http: what goes to it can be read on the way.
    pub in_the_clear: bool,
}

/// What one server said its marketplace holds.
#[derive(Default)]
pub(super) struct RemoteMarket {
    pub(super) catalog: Option<Vec<PluginEntry>>,
    /// The names of the MCP servers Claude has there.
    servers: Vec<String>,
    /// A `market` was asked for and has not come back.
    loading: bool,
    /// Something was installed while that `market` was on the way, so its
    /// answer is old when it comes.
    outdated: bool,
    at: Option<Instant>,
    pub(super) error: Option<SharedString>,
    /// Why it turned down what Discover asked, such as an older server not
    /// knowing the message. About the server, so the MCP list shows it too.
    refused: Option<SharedString>,
    /// The install it is doing for us.
    pending: Option<Pending>,
}

impl RemoteMarket {
    /// An install there worked, so what it said is old.
    fn changed(&mut self) {
        self.at = None;
        self.outdated = self.loading;
    }

    /// Reads a `market` answer. False when it was asked before the last
    /// install there, so it has to be asked again.
    fn read(&mut self, msg: &Value) -> bool {
        self.loading = false;
        self.at = Some(Instant::now());
        match serde_json::from_value::<Vec<PluginEntry>>(msg["plugins"].clone()) {
            Ok(list) => self.catalog = Some(list),
            Err(e) => self.error = Some(format!("Could not read its answer: {e}").into()),
        }
        self.servers = msg["servers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        if let Some(e) = msg["error"].as_str() {
            self.error = Some(e.to_string().into());
        }
        if std::mem::take(&mut self.outdated) {
            self.at = None;
            return false;
        }
        true
    }
}

/// An install sent to a server, waiting for its `market_done`.
struct Pending {
    /// The plugin id or server name it was sent with, which the answer names.
    key: String,
    /// What the user knows it as.
    name: String,
    /// What the notice says once it worked.
    done: String,
}

/// The parts of `ExtensionsView` this module keeps.
#[derive(Default)]
pub(super) struct Targets {
    /// Where Discover installs: this computer, or a paired server's id.
    pub(super) target: Option<String>,
    /// The paired servers, as the shell last told.
    pub(super) servers: Vec<ServerTarget>,
    pub(super) remote: HashMap<String, RemoteMarket>,
}

impl Targets {
    /// Forgets what the servers no longer in `servers` said. Gives the name of
    /// an install one of them was doing: its answer will not come.
    fn retire(&mut self, servers: &[ServerTarget]) -> Option<(String, String)> {
        let gone: Vec<String> = self
            .remote
            .keys()
            .filter(|id| !servers.iter().any(|s| s.id == **id))
            .cloned()
            .collect();
        let mut lost = None;
        for id in gone {
            if let Some(p) = self.remote.remove(&id).and_then(|r| r.pending) {
                let name = self
                    .servers
                    .iter()
                    .find(|s| s.id == id)
                    .map_or_else(|| "the server".to_string(), |s| s.name.clone());
                lost = Some((name, p.name));
            }
        }
        lost
    }
}

/// Whether a server's `error` is about what Discover asked. The server answers
/// those with `market` and `market_done`; only one that cannot read the
/// message says `error`, without saying what about. Errors about sessions and
/// the rest stay with the shell.
fn about_market(msg: &Value) -> bool {
    let message = msg["message"].as_str().unwrap_or("");
    if !msg["about"].is_null() || !message.starts_with("bad message") {
        return false;
    }
    match message.strip_prefix("bad message: unknown variant `") {
        Some(rest) => ["market`", "install_plugin`", "install_server`"]
            .iter()
            .any(|k| rest.starts_with(k)),
        None => true,
    }
}

impl ExtensionsView {
    /// The paired servers and how each is reached: Discover offers to install on them.
    pub fn set_servers(&mut self, servers: Vec<ServerTarget>, cx: &mut Context<Self>) {
        if self.targets.servers == servers {
            return;
        }
        if let Some(id) = &self.targets.target
            && !servers.iter().any(|s| s.id == *id)
        {
            self.pick_target(None, cx);
        }
        // A server forgotten while it installs: its answer will not come.
        if let Some((server, name)) = self.targets.retire(&servers) {
            self.op = None;
            self.error = Some(
                format!(
                    "{server} was removed while installing {}. Whether it was installed there is not known.",
                    display_name(&name)
                )
                .into(),
            );
        }
        for s in servers.iter().filter(|s| s.reach != Reach::Connected) {
            let Some(r) = self.targets.remote.get_mut(&s.id) else {
                continue;
            };
            // What was asked will not be answered: ask again once it is back.
            r.loading = false;
            if let Some(p) = r.pending.take() {
                self.op = None;
                self.error = Some(
                    format!(
                        "Lost {} while installing {}. Look again once it is back.",
                        s.name,
                        display_name(&p.name)
                    )
                    .into(),
                );
            }
        }
        self.targets.servers = servers;
        self.ask_remote(false, cx);
        cx.notify();
    }

    /// The server Discover installs on, when it is not this computer.
    pub(super) fn target_server(&self) -> Option<&ServerTarget> {
        let id = self.targets.target.as_ref()?;
        self.targets.servers.iter().find(|s| s.id == *id)
    }

    fn target_market(&self) -> Option<&RemoteMarket> {
        self.targets.remote.get(self.targets.target.as_ref()?)
    }

    /// Where Install goes, for the words around it: "on <server>" or "".
    pub(super) fn on_target(&self) -> String {
        self.target_server()
            .map(|s| format!(" on {}", s.name))
            .unwrap_or_default()
    }

    /// The plugins Discover lists: this computer's, or the picked server's.
    pub(super) fn plugin_catalog(&self) -> Option<&Vec<PluginEntry>> {
        match &self.targets.target {
            None => self.ms.catalog.as_ref(),
            Some(_) => self.target_market()?.catalog.as_ref(),
        }
    }

    pub(super) fn catalog_loading(&self) -> bool {
        match &self.targets.target {
            None => self.ms.catalog_loading,
            Some(_) => self.target_market().is_some_and(|r| r.loading),
        }
    }

    /// Why the plugins could not be read, or why a server cannot be asked.
    pub(super) fn catalog_error(&self) -> Option<SharedString> {
        if self.targets.target.is_none() {
            return self.ms.catalog_error.clone();
        }
        self.server_refused()
            .or_else(|| self.target_market().and_then(|r| r.error.clone()))
    }

    /// Why the picked server cannot do what Discover asks: it is out of
    /// reach, or turned the request down.
    pub(super) fn server_refused(&self) -> Option<SharedString> {
        self.reach_error()
            .or_else(|| self.target_market().and_then(|r| r.refused.clone()))
    }

    /// Why the picked server cannot be asked anything right now.
    fn reach_error(&self) -> Option<SharedString> {
        let server = self.target_server()?;
        match &server.reach {
            Reach::Away(why) | Reach::Refused(why) => {
                Some(format!("{} cannot be reached: {why}", server.name).into())
            }
            _ => None,
        }
    }

    /// What the list says while the plugins are on their way.
    pub(super) fn catalog_wait(&self) -> String {
        match self.target_server() {
            None => "Reading the plugin marketplaces…".into(),
            Some(s) if s.reach == Reach::Connected => {
                format!("Reading the plugin marketplaces on {}…", s.name)
            }
            Some(s) => format!("Connecting to {}…", s.name),
        }
    }

    /// Whether Claude on the picked server has an MCP server of that name.
    pub(super) fn remote_has_server(&self, name: &str) -> Option<bool> {
        self.targets.target.as_ref()?;
        Some(
            self.target_market()
                .is_some_and(|r| r.servers.iter().any(|s| s == name)),
        )
    }

    /// Discover installs on this computer (None) or on a paired server.
    pub(super) fn pick_target(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        if self.kind != Kind::Marketplace {
            self.show_market(self.market, cx);
        }
        if self.targets.target == target {
            return;
        }
        self.targets.target = target;
        self.ms.category = None;
        self.ms.selected = None;
        self.ms.installing = None;
        self.ms.rows.top();
        (self.error, self.notice) = (None, None);
        if self.targets.target.is_some() {
            // Nothing connects to a server until something needs it.
            cx.emit(ExtensionsEvent::ConnectServers);
        }
        self.open_market(cx);
        cx.notify();
    }

    /// Asks the picked server for its marketplace, when it is connected and
    /// what it said is missing or old (or `again`).
    pub(super) fn ask_remote(&mut self, again: bool, cx: &mut Context<Self>) {
        let Some(server) = self.target_server() else {
            return;
        };
        if server.reach != Reach::Connected {
            return;
        }
        let id = server.id.clone();
        let r = self.targets.remote.entry(id.clone()).or_default();
        let stale = r.at.is_none_or(|t| t.elapsed() > STALE);
        if r.loading || !(again || stale) {
            return;
        }
        r.loading = true;
        r.error = None;
        r.refused = None;
        cx.emit(ExtensionsEvent::Server(id, json!({ "type": "market" })));
        cx.notify();
    }

    /// Sends an install to the picked server. `key` is what its answer names.
    /// False when it was not sent.
    pub(super) fn install_remote(
        &mut self,
        name: String,
        key: String,
        msg: Value,
        done: String,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.op.is_some() {
            return false;
        }
        let Some(server) = self.target_server().cloned() else {
            return false;
        };
        (self.error, self.notice) = (None, None);
        if server.reach != Reach::Connected {
            self.error = Some(format!("{} is not connected right now.", server.name).into());
            cx.notify();
            return false;
        }
        let id = server.id;
        self.op = Some(name.clone());
        self.targets.remote.entry(id.clone()).or_default().pending =
            Some(Pending { key, name, done });
        cx.emit(ExtensionsEvent::Server(id, msg));
        cx.notify();
        true
    }

    /// A server's answer to what Discover asked of it.
    pub fn server_said(&mut self, server: &str, msg: &Value, cx: &mut Context<Self>) {
        let Some(r) = self.targets.remote.get_mut(server) else {
            return;
        };
        match msg["type"].as_str().unwrap_or("") {
            "market" => {
                if !r.read(msg) && self.targets.target.as_deref() == Some(server) {
                    self.ask_remote(false, cx);
                }
            }
            "market_done" => {
                let key = msg["name"].as_str().unwrap_or("");
                if r.pending.as_ref().is_none_or(|p| p.key != key) {
                    return;
                }
                let Some(p) = r.pending.take() else { return };
                self.op = None;
                match msg["error"].as_str() {
                    Some(e) => self.error = Some(format!("{}: {e}", display_name(&p.name)).into()),
                    None => {
                        self.notice = Some(format!("{} {}", display_name(&p.name), p.done).into());
                        // What that server said is old now, even when another is
                        // picked or a `market` is already on the way.
                        r.changed();
                        self.ask_remote(false, cx);
                    }
                }
            }
            _ => return,
        }
        cx.notify();
    }

    /// An error a server sent. Taken (true) when it is about what Discover
    /// asked and Discover was waiting on that server.
    pub fn server_error(&mut self, server: &str, msg: &Value, cx: &mut Context<Self>) -> bool {
        if !about_market(msg) {
            return false;
        }
        let message = msg["message"].as_str().unwrap_or("");
        let name = self
            .targets
            .servers
            .iter()
            .find(|s| s.id == server)
            .map_or("The server".to_string(), |s| s.name.clone());
        let Some(r) = self.targets.remote.get_mut(server) else {
            return false;
        };
        if !r.loading && r.pending.is_none() {
            return false;
        }
        // A server from before this message existed does not know it.
        let why: SharedString = if message.starts_with("bad message") {
            format!(
                "{name} runs an older arakun server, which cannot install from here. Update it there."
            )
            .into()
        } else {
            message.to_string().into()
        };
        if r.loading {
            r.loading = false;
            r.outdated = false;
            r.at = Some(Instant::now());
            r.refused = Some(why.clone());
        }
        if r.pending.take().is_some() {
            self.op = None;
            self.error = Some(why);
        }
        cx.notify();
        true
    }

    /// Above Discover's sources, when servers are paired: where Install goes.
    pub(super) fn target_lines(&self) -> Vec<Line> {
        if self.targets.servers.is_empty() {
            return Vec::new();
        }
        let mut out = vec![
            Line::Heading("Install on".into()),
            Line::Entry(
                Entry::new("target:", Lead::Icon("home01"), "This computer")
                    .on(self.targets.target.is_none()),
            ),
        ];
        for s in &self.targets.servers {
            let on = self.targets.target.as_deref() == Some(s.id.as_str());
            out.push(Line::Entry(
                Entry::new(
                    format!("target:{}", s.id),
                    Lead::Icon("computer"),
                    s.name.clone(),
                )
                .on(on)
                .dim(s.reach != Reach::Connected),
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: &str) -> ServerTarget {
        ServerTarget {
            id: id.into(),
            name: format!("server {id}"),
            reach: Reach::Connected,
            in_the_clear: false,
        }
    }

    #[test]
    fn only_errors_about_the_marketplace_are_taken() {
        let old = |m: &str| json!({ "type": "error", "message": m });
        assert!(about_market(&old(
            "bad message: unknown variant `market`, expected one of `open`"
        )));
        assert!(about_market(&old(
            "bad message: unknown variant `install_server`, expected one of `open`"
        )));
        assert!(about_market(&old("bad message: missing field `id`")));
        // Errors about sessions, starts and other requests are the shell's.
        assert!(!about_market(&old("no such session")));
        assert!(!about_market(&old("that project is gone")));
        assert!(!about_market(&old(
            "bad message: unknown variant `settle_env`, expected one of `open`"
        )));
        assert!(!about_market(
            &json!({ "type": "error", "about": "start", "message": "bad message: x" })
        ));
    }

    #[test]
    fn a_removed_server_gives_up_its_install() {
        let mut t = Targets {
            servers: vec![target("a"), target("b")],
            ..Default::default()
        };
        for id in ["a", "b"] {
            t.remote.entry(id.into()).or_default().pending = Some(Pending {
                key: "k".into(),
                name: format!("plugin {id}"),
                done: String::new(),
            });
        }
        let lost = t.retire(&[target("b")]);
        assert_eq!(lost, Some(("server a".into(), "plugin a".into())));
        assert!(!t.remote.contains_key("a"));
        // The one still there keeps waiting for its answer.
        assert!(t.remote["b"].pending.is_some());
        assert_eq!(t.retire(&[target("b")]), None);
    }

    #[test]
    fn an_install_during_a_load_asks_again() {
        let mut r = RemoteMarket {
            loading: true,
            ..Default::default()
        };
        let before = json!({ "type": "market", "plugins": [], "servers": [] });
        // The install's `market_done` comes before the `market` sent earlier.
        r.changed();
        assert!(!r.read(&before));
        assert!(r.at.is_none());
        r.loading = true;
        assert!(r.read(&before));
        assert!(r.at.is_some());
        // With nothing on the way, the next ask reads it fresh.
        r.changed();
        assert!(r.at.is_none() && !r.outdated);
    }
}
