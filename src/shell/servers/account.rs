//! The account's servers: machines signed in to the same account, which the
//! app reaches through the relay with its own sign-in, with no link.
//!
//! The account lists them (`cloud::machines`) with their keys. A server
//! added from that list is kept in `servers.json` by its label only; it is
//! connected once the list is here, so each connection checks the server
//! against the key the account lists now. A server paired through the relay
//! before accounts is moved over once the account lists its label.

use super::*;
use crate::cloud;
use crate::servers::client::Account;
use crate::servers::{Way, account_label, adopt};
use crate::servers_view::{MachineRow, YourServers};

/// The account's machines, as last asked.
#[derive(Default)]
pub(in crate::shell) struct Machines {
    /// The service and session the list was asked with. A new sign-in asks
    /// again; an answer for another one is dropped.
    asked: Option<(String, String)>,
    /// What the account listed, once it answered.
    list: Option<Vec<cloud::Machine>>,
    error: Option<String>,
    _task: Option<Task<()>>,
    /// The sign-in the connections through the account were started with.
    /// Apart from `asked`, which a new list overwrites: a sign-out stops
    /// those connections whichever is told first.
    started: Option<(String, String)>,
    /// This computer's label on the relay, read once: never listed.
    own_label: Option<String>,
}

impl Machines {
    /// The list is asked again with `session`.
    fn ask(&mut self, session: Option<(String, String)>) {
        self.asked = session;
    }

    /// Whether the connections through the account were started with another
    /// sign-in than `now`, and must stop. They count as stopped after.
    fn stop_if_changed(&mut self, now: &Option<(String, String)>) -> bool {
        if self.started.is_some() && self.started != *now {
            self.started = None;
            return true;
        }
        false
    }
}

/// The sign-in servers are dialed with: none in a run with the made-up
/// account of `ARAKUN_TEAMS_FIXTURE`, which must never reach a real relay.
fn dial_session(session: Option<(String, String)>, fixture: bool) -> Option<(String, String)> {
    session.filter(|_| !fixture)
}

/// The servers the account lists, for the Servers tab: of kind `server`,
/// never this computer, each marked when it is in the list here already.
pub(super) fn your_servers(
    machines: &Machines,
    saved: &[Saved],
    own_label: Option<&str>,
) -> YourServers {
    if machines.asked.is_none() {
        return YourServers::SignedOut;
    }
    if let Some(e) = &machines.error {
        return YourServers::Failed(e.clone());
    }
    let Some(list) = &machines.list else {
        return YourServers::Loading;
    };
    YourServers::Listed(
        list.iter()
            .filter(|m| m.kind == "server" && Some(m.label.as_str()) != own_label)
            .map(|m| MachineRow {
                label: m.label.clone(),
                name: m.name.clone(),
                online: m.online,
                added: saved
                    .iter()
                    .any(|s| s.label() == Some(m.label.as_str()) && !s.needs_sign_in()),
            })
            .collect(),
    )
}

/// The machines the made-up account of `ARAKUN_TEAMS_FIXTURE` lists, so a
/// test run shows the lists with rows in them: this computer when it has a
/// key, another computer and a server. Nothing dials them.
pub(in crate::shell) fn fixture_machines() -> Vec<cloud::Machine> {
    let machine = |label: String, name: &str, kind: &str, online: bool| cloud::Machine {
        label,
        public: String::new(),
        name: name.into(),
        kind: kind.into(),
        online,
        last_seen_at: crate::model::unix_now().saturating_sub(3 * 3600),
    };
    let mut list = Vec::new();
    if let Some(own) = own_label() {
        list.push(machine(own, &crate::phone::host_name(), "app", true));
    }
    list.push(machine(
        "abcdefghijklmnop".into(),
        "Studio Mac",
        "app",
        false,
    ));
    list.push(machine(
        "qrstuvwxyz234567".into(),
        "build-box",
        "server",
        true,
    ));
    list
}

/// Whether a new list of the account's servers dials a server again, given
/// how its connection stands (`None`: there is none yet). Not one whose user
/// removed this app there, or the server would take this app back.
fn dial_again(reach: Option<&Reach>) -> bool {
    !reach.is_some_and(removed_by_server)
}

/// Whether the server turned this app away because its user removed it.
fn removed_by_server(reach: &Reach) -> bool {
    matches!(reach, Reach::Refused(why) if why == crate::servers::client::UNPAIRED_BY_SERVER)
}

/// This computer's label on the relay, when it has a key yet. Reads
/// `phone.json`: asked when the list is, not on every view update.
pub(super) fn own_label() -> Option<String> {
    crate::phone::Settings::load().relay_label()
}

impl Shell {
    /// What the Servers tab lists under "Your servers".
    pub(super) fn servers_yours(&self) -> YourServers {
        let Some(link) = &self.servers else {
            return YourServers::SignedOut;
        };
        your_servers(
            &link.machines,
            &link.saved,
            link.machines.own_label.as_deref(),
        )
    }

    /// The sign-in changed (in, out, or to another account): the servers of
    /// the old one are let go, and the new one is asked for its list.
    pub(in crate::shell) fn servers_account_changed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self.teams.read(cx).session();
        let fixture = crate::arakun_teams::fixture().is_some();
        let Some(link) = &mut self.servers else {
            return;
        };
        // Their connections were made with the old sign-in: they stop, and
        // wait for the new one's list.
        if link
            .machines
            .stop_if_changed(&dial_session(session.clone(), fixture))
        {
            let theirs: Vec<Saved> = link
                .saved
                .iter()
                .filter(|s| matches!(s.way, Way::Account { .. }))
                .cloned()
                .collect();
            for s in theirs {
                if let Some(conn) = link.conns.get_mut(&s.id) {
                    conn.client = Client::connect(s, link.events.clone());
                }
            }
        }
        if link.machines.asked != session {
            link.machines.list = None;
            link.machines.error = None;
            self.servers_list_machines(window, cx);
        }
        self.sync_servers_view(cx);
    }

    /// This computer got a new key, so a new label on the relay.
    pub(in crate::shell) fn servers_own_label_changed(&mut self) {
        if let Some(link) = &mut self.servers {
            link.machines.own_label = own_label();
        }
    }

    /// Asks the account for its machines, when signed in.
    pub(in crate::shell) fn servers_list_machines(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self.teams.read(cx).session();
        let Some(link) = &mut self.servers else {
            return;
        };
        link.machines.ask(session.clone());
        link.machines.own_label = own_label();
        let Some((base, token)) = session else {
            return;
        };
        let asked = (base.clone(), token.clone());
        link.machines._task = Some(cx.spawn_in(window, async move |this, cx| {
            let listed = cx
                .background_spawn(async move {
                    // The fixture's session is made up: the service would
                    // answer 401 and sign it out.
                    if crate::arakun_teams::fixture().is_some() {
                        return Ok(fixture_machines());
                    }
                    cloud::machines(&base, &token)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.servers_machines_listed(asked, listed, window, cx)
            });
        }));
    }

    fn servers_machines_listed(
        &mut self,
        asked: (String, String),
        listed: Result<Vec<cloud::Machine>, cloud::Error>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = &mut self.servers else {
            return;
        };
        // Asked with a sign-in that has changed since.
        if link.machines.asked.as_ref() != Some(&asked) {
            return;
        }
        link.machines._task = None;
        match listed {
            Ok(list) => {
                link.machines.error = None;
                let labels: Vec<String> = list.iter().map(|m| m.label.clone()).collect();
                if adopt(&mut link.saved, &labels, &asked.0) {
                    servers::save(&link.saved);
                }
                link.machines.list = Some(list);
                self.servers_reach_accounts(window, cx);
            }
            // The stored sign-in is dead: it is forgotten, and the prompt shows.
            Err(cloud::Error::SignedOut) => {
                self.teams.update(cx, |t, cx| t.session_ended(cx));
            }
            Err(e) => link.machines.error = Some(e.to_string()),
        }
        self.sync_servers_view(cx);
    }

    /// Connects each server of the account in the list here that the
    /// account lists, with the key it lists for it.
    fn servers_reach_accounts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.teams.read(cx).session();
        let fixture = crate::arakun_teams::fixture().is_some();
        let Some(link) = &mut self.servers else {
            return;
        };
        // Only with the sign-in the list was asked with, and never the made-up one.
        let Some((base, token)) = dial_session(session, fixture) else {
            return;
        };
        if link.machines.asked.as_ref() != Some(&(base.clone(), token.clone())) {
            return;
        }
        let Some(list) = &link.machines.list else {
            return;
        };
        link.machines.started = Some((base.clone(), token.clone()));
        let mut late = Vec::new();
        for s in &link.saved {
            if !matches!(s.way, Way::Account { .. }) {
                continue;
            }
            // Decided before any client exists: a client made and dropped
            // would still start dialing, and its status would overwrite the
            // refusal, so the next list would dial again.
            if !dial_again(link.conns.get(&s.id).map(|c| &c.reach)) {
                continue;
            }
            let client = match account_label(s, &base) {
                Err(why) => Client::held(s, why, link.events.clone()),
                Ok(label) => {
                    let Some(machine) = list.iter().find(|m| m.label == label) else {
                        continue;
                    };
                    let account = Account {
                        base: base.clone(),
                        session: token.clone(),
                        public: machine.public.clone(),
                    };
                    Client::connect_account(s.clone(), account, link.events.clone())
                }
            };
            match link.conns.get_mut(&s.id) {
                Some(conn) => {
                    conn.client = client;
                    conn.reach = Reach::Connecting;
                }
                None => {
                    link.conns.insert(s.id.clone(), Conn::new(client));
                    late.extend(link.inboxes.get(&s.id).map(|i| (s.id.clone(), i.clone())));
                }
            }
        }
        for (id, inbox) in &late {
            self.servers_inbox(id, inbox);
        }
        if !late.is_empty() {
            self.sync_remote_projects(window, cx);
        }
    }

    /// Removes one of the account's servers from the account, then asks
    /// for the list again.
    pub(super) fn servers_remove_machine(
        &mut self,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The fixture's session is made up: the service would sign it out.
        let session = self.teams.read(cx).session();
        let Some((base, token)) = dial_session(session, crate::arakun_teams::fixture().is_some())
        else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let done = cx
                .background_spawn(async move { cloud::forget_machine(&base, &token, &label) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match done {
                    Ok(()) => {}
                    Err(cloud::Error::SignedOut) => {
                        this.teams.update(cx, |t, cx| t.session_ended(cx));
                    }
                    Err(e) => {
                        if let Some(link) = &mut this.servers {
                            link.error = Some(format!("Could not remove the server: {e}"));
                        }
                    }
                }
                this.servers_list_machines(window, cx);
                this.sync_servers_view(cx);
            });
        })
        .detach();
    }

    /// "Connect" on one of the account's servers: it joins the list here,
    /// and is connected with the key the account lists.
    pub(super) fn servers_add_machine(
        &mut self,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = &mut self.servers else {
            return;
        };
        let (Some((base, _)), Some(list)) = (&link.machines.asked, &link.machines.list) else {
            return;
        };
        let Some(machine) = list.iter().find(|m| m.label == label) else {
            return;
        };
        if !link.saved.iter().any(|s| s.label() == Some(label)) {
            link.saved.push(Saved {
                id: servers::new_id(),
                name: machine.name.clone(),
                origin: base.clone(),
                way: Way::Account {
                    label: label.to_string(),
                },
                device: String::new(),
                token: String::new(),
            });
            servers::save(&link.saved);
        }
        self.servers_reach_accounts(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(label: &str, kind: &str) -> cloud::Machine {
        cloud::Machine {
            label: label.into(),
            public: String::new(),
            name: format!("{label} box"),
            kind: kind.into(),
            online: true,
            last_seen_at: 0,
        }
    }

    fn signed(token: &str) -> Option<(String, String)> {
        Some(("https://app.arakun.dev".into(), token.into()))
    }

    #[test]
    fn a_sign_out_stops_the_account_connections_whichever_comes_first() {
        // The list is asked again (the tab opened) before the change is told.
        let mut m = Machines {
            started: signed("old"),
            ..Machines::default()
        };
        m.ask(None);
        assert!(m.stop_if_changed(&None));
        assert!(m.started.is_none());
        // The change is told first.
        let mut m = Machines {
            started: signed("old"),
            ..Machines::default()
        };
        assert!(m.stop_if_changed(&None));
        m.ask(None);
        assert!(m.started.is_none());
        // Another account: they stop too. The same sign-in again: they stay.
        let mut m = Machines {
            started: signed("old"),
            ..Machines::default()
        };
        assert!(!m.stop_if_changed(&signed("old")));
        assert!(m.stop_if_changed(&signed("new")));
        assert!(!m.stop_if_changed(&None), "nothing left to stop");
    }

    #[test]
    fn a_server_that_removed_this_app_is_not_dialed_again() {
        let removed = Reach::Refused(crate::servers::client::UNPAIRED_BY_SERVER.into());
        assert!(removed_by_server(&removed));
        assert!(!removed_by_server(&Reach::Refused("signed out".into())));
        assert!(!removed_by_server(&Reach::Connected));
        // Decided from the connection alone, before a client is made.
        assert!(!dial_again(Some(&removed)));
        assert!(dial_again(Some(&Reach::Refused("signed out".into()))));
        assert!(dial_again(Some(&Reach::Connecting)));
        assert!(dial_again(None));
    }

    #[test]
    fn the_made_up_account_of_a_test_run_dials_nothing() {
        assert_eq!(dial_session(signed("tok"), true), None);
        assert_eq!(dial_session(signed("tok"), false), signed("tok"));
        assert_eq!(dial_session(None, false), None);
    }

    #[test]
    fn your_servers_are_the_accounts_servers_but_this_computer() {
        let mut machines = Machines::default();
        assert!(matches!(
            your_servers(&machines, &[], None),
            YourServers::SignedOut
        ));
        machines.asked = Some(("https://app.arakun.dev".into(), "tok".into()));
        assert!(matches!(
            your_servers(&machines, &[], None),
            YourServers::Loading
        ));
        machines.list = Some(vec![
            machine("aaaaaaaaaaaaaaaa", "server"),
            machine("bbbbbbbbbbbbbbbb", "app"),
            machine("cccccccccccccccc", "server"),
            machine("dddddddddddddddd", "server"),
        ]);
        let saved = vec![Saved {
            id: "s1".into(),
            name: "c".into(),
            origin: "https://app.arakun.dev".into(),
            way: Way::Account {
                label: "cccccccccccccccc".into(),
            },
            device: String::new(),
            token: String::new(),
        }];
        let YourServers::Listed(rows) = your_servers(&machines, &saved, Some("dddddddddddddddd"))
        else {
            panic!("listed");
        };
        let shown: Vec<(&str, bool)> = rows.iter().map(|r| (r.label.as_str(), r.added)).collect();
        assert_eq!(
            shown,
            [("aaaaaaaaaaaaaaaa", false), ("cccccccccccccccc", true)]
        );
        machines.error = Some("offline".into());
        assert!(matches!(
            your_servers(&machines, &saved, None),
            YourServers::Failed(_)
        ));
    }
}
