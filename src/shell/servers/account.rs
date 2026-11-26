//! The account's servers: machines signed in to the same account, which the
//! app reaches through the relay with its own sign-in, with no link.
//!
//! The account lists them (`cloud::machines`) with their keys. Each server
//! it lists joins the list here by itself, kept in `servers.json` by its
//! label only; it is connected once the list is here, so each connection
//! checks the server against the key the account lists now. A server paired
//! through the relay before accounts is moved over once the account lists
//! its label.

use super::*;
use crate::cloud;
use crate::servers::client::{
    ACCOUNT_UNREACHABLE, Account, NOT_LISTED, SIGN_IN_TO_RECONNECT, UNPAIRED_BY_SERVER,
};
use crate::servers::{Way, account_label, adopt};
use crate::servers_view::{MachineRow, YourServers};
use std::time::Duration;

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
    /// The list is asked again after a failure, while a server waits on it.
    _retry: Option<Task<()>>,
    /// How long the next retry waits; zero stands for the first wait.
    retry_after: Duration,
    /// This computer's label on the relay, read once: never listed.
    own_label: Option<String>,
}

/// The first wait before the list is asked again after a failure; each
/// failure doubles it up to `LAST_RETRY`.
const FIRST_RETRY: Duration = Duration::from_secs(2);
const LAST_RETRY: Duration = Duration::from_secs(60);

impl Machines {
    /// The list is asked again with `session`.
    fn ask(&mut self, session: Option<(String, String)>) {
        self.asked = session;
    }

    /// How long to wait before asking again after a failure. Each call
    /// doubles the next wait, up to a minute.
    fn next_retry(&mut self) -> Duration {
        let wait = self.retry_after.max(FIRST_RETRY);
        self.retry_after = (wait * 2).min(LAST_RETRY);
        wait
    }

    /// The list answered, or is asked for another reason than a retry.
    fn stop_retrying(&mut self) {
        self._retry = None;
        self.retry_after = Duration::ZERO;
    }
}

/// Whether a failed list is worth asking again by itself: only while a
/// server here is reached through the relay, so waits on the list.
fn waits_on_list(saved: &[Saved]) -> bool {
    saved.iter().any(|s| s.label().is_some())
}

/// What the connection to a server of the account was started with.
#[derive(Clone, PartialEq)]
pub(in crate::shell) enum Start {
    /// Dialed with this sign-in and the key the account listed.
    Dialed(Account),
    /// Not dialed: it only says this.
    Held(LinkStatus),
}

impl Start {
    /// How the connection stands as it starts, before its client says so:
    /// a held one shows its sentence at once, not "Connecting" first.
    pub(in crate::shell) fn reach(&self) -> Reach {
        match self {
            Start::Held(LinkStatus::Refused(why)) => Reach::Refused(why.clone()),
            Start::Held(LinkStatus::Waiting { why, .. }) => Reach::Away(why.clone()),
            _ => Reach::Connecting,
        }
    }
}

/// What a server of the account is to be started with now: dialed when the
/// signed-in account lists it, and otherwise held with the sentence that
/// says what it waits for. `session` is the sign-in servers are dialed with.
fn start_for(s: &Saved, session: Option<&(String, String)>, machines: &Machines) -> Start {
    let refused = |why: &str| Start::Held(LinkStatus::Refused(why.to_string()));
    // Its user removed this app there: dialing would have it take the app back.
    if s.removed {
        return refused(UNPAIRED_BY_SERVER);
    }
    let Some((base, token)) = session else {
        return refused(SIGN_IN_TO_RECONNECT);
    };
    // The list of this sign-in is still on its way, or could not be had.
    let list = match &machines.list {
        Some(list) if machines.asked.as_ref() == session => list,
        _ if machines.asked.as_ref() == session && machines.error.is_some() => {
            return Start::Held(LinkStatus::Waiting {
                why: ACCOUNT_UNREACHABLE.into(),
                retry_in: Duration::ZERO,
            });
        }
        _ => return Start::Held(LinkStatus::Connecting),
    };
    let label = match account_label(s, base) {
        Ok(label) => label,
        Err(why) => return refused(&why),
    };
    match list.iter().find(|m| m.label == label) {
        Some(machine) => Start::Dialed(Account {
            base: base.clone(),
            session: token.clone(),
            public: machine.public.clone(),
        }),
        None => refused(NOT_LISTED),
    }
}

/// Whether a server of the account gets a new connection, given how the one
/// it has stands (`None`: it has none) and what that one was started with.
/// One that is connected or connecting with the same sign-in and key is left
/// alone: hanging up would lose what it was asked and has not answered.
fn starts_again(now: &Start, was: Option<&Start>, reach: Option<&Reach>) -> bool {
    let Some(reach) = reach else {
        return true;
    };
    if was != Some(now) {
        return true;
    }
    // The same as before. A dialed one retries a drop by itself; one that
    // was refused gave up, and a new list is the moment to try once more.
    matches!((now, reach), (Start::Dialed(_), Reach::Refused(_)))
}

/// The client for a server of the account, as `start` says.
fn account_client(s: &Saved, start: &Start, events: Events) -> Client {
    match start {
        Start::Dialed(account) => Client::connect_account(s.clone(), account.clone(), events),
        Start::Held(status) => Client::held(s, status.clone(), events),
    }
}

/// The sign-in servers are dialed with: none in a run with the made-up
/// account of `ARAKUN_TEAMS_FIXTURE`, which must never reach a real relay.
fn dial_session(session: Option<(String, String)>, fixture: bool) -> Option<(String, String)> {
    session.filter(|_| !fixture)
}

/// The servers the account lists, for the Computers tab: of kind `server`,
/// never this computer.
pub(super) fn your_servers(machines: &Machines, own_label: Option<&str>) -> YourServers {
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
                last_seen: m.last_seen_at,
            })
            .collect(),
    )
}

/// The servers the account lists that are not in the list here yet, to be
/// kept as reached through the account at `base`: a server signed in to the
/// same account needs no step here. Never this computer, and never one that
/// looks paired already by link (a server of the same name reached
/// directly), which would show twice.
pub(super) fn joining(
    list: &[cloud::Machine],
    saved: &[Saved],
    own_label: Option<&str>,
    base: &str,
) -> Vec<Saved> {
    list.iter()
        .filter(|m| m.kind == "server" && Some(m.label.as_str()) != own_label)
        .filter(|m| !saved.iter().any(|s| s.label() == Some(m.label.as_str())))
        .filter(|m| {
            !saved
                .iter()
                .any(|s| matches!(s.way, Way::Direct) && s.name.eq_ignore_ascii_case(&m.name))
        })
        .map(|m| Saved {
            id: servers::new_id(),
            name: m.name.clone(),
            origin: base.to_string(),
            way: Way::Account {
                label: m.label.clone(),
            },
            device: String::new(),
            token: Default::default(),
            key: String::new(),
            removed: false,
        })
        .collect()
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

/// This computer's label on the relay, when it has a key yet. Reads
/// `phone.json`: asked when the list is, not on every view update.
pub(super) fn own_label() -> Option<String> {
    crate::phone::Settings::load().relay_label()
}

impl Shell {
    /// The servers the account lists, as the Computers tab shows them.
    pub(super) fn servers_yours(&self) -> YourServers {
        let Some(link) = &self.servers else {
            return YourServers::SignedOut;
        };
        your_servers(&link.machines, link.machines.own_label.as_deref())
    }

    /// The sign-in changed (in, out, or to another account): the servers of
    /// the old one are let go, and the new one is asked for its list.
    pub(in crate::shell) fn servers_account_changed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self.teams.read(cx).session();
        let Some(link) = &mut self.servers else {
            return;
        };
        if link.machines.asked != session {
            link.machines.list = None;
            link.machines.error = None;
            self.servers_list_machines(window, cx);
        }
        // The connections made with the old sign-in stop, and wait for the
        // new one's list. With the same sign-in, none is touched.
        self.servers_reach_accounts(window, cx);
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
        // Asked for a reason of its own: a retry under way starts over.
        if let Some(link) = &mut self.servers {
            link.machines.stop_retrying();
        }
        self.servers_ask_machines(window, cx);
    }

    fn servers_ask_machines(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                link.machines.stop_retrying();
                let labels: Vec<String> = list.iter().map(|m| m.label.clone()).collect();
                let mut changed = adopt(&mut link.saved, &labels, &asked.0);
                let new = joining(
                    &list,
                    &link.saved,
                    link.machines.own_label.as_deref(),
                    &asked.0,
                );
                changed |= !new.is_empty();
                link.saved.extend(new);
                if changed {
                    servers::save(&link.saved);
                }
                link.machines.list = Some(list);
                self.servers_reach_accounts(window, cx);
            }
            // The stored sign-in is dead: it is forgotten, and the prompt shows.
            Err(cloud::Error::SignedOut) => {
                self.teams.update(cx, |t, cx| t.session_ended(cx));
            }
            Err(e) => {
                link.machines.error = Some(e.to_string());
                // Nothing else asks again: not the tab, which may never be
                // opened, and not the servers' own connections, which wait
                // for this list before they dial.
                if waits_on_list(&link.saved) {
                    let wait = link.machines.next_retry();
                    link.machines._retry = Some(cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor().timer(wait).await;
                        let _ = this.update_in(cx, |this, window, cx| {
                            this.servers_ask_machines(window, cx)
                        });
                    }));
                }
                // The servers waiting on the list say why they still wait.
                self.servers_reach_accounts(window, cx);
            }
        }
        self.sync_servers_view(cx);
    }

    /// Brings each server of the account in the list here to where it
    /// should be now: dialed with the key the account lists, or held with
    /// what it waits for. One already there is left alone.
    pub(super) fn servers_reach_accounts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.servers_dial_session(cx);
        let Some(link) = &mut self.servers else {
            return;
        };
        let mut late = Vec::new();
        for s in &link.saved {
            if !matches!(s.way, Way::Account { .. }) {
                continue;
            }
            let now = start_for(s, session.as_ref(), &link.machines);
            // Decided before any client exists: a client made and dropped
            // would still start, and its status would overwrite this one's.
            let conn = link.conns.get(&s.id);
            if !starts_again(
                &now,
                conn.and_then(|c| c.start.as_ref()),
                conn.map(|c| &c.reach),
            ) {
                continue;
            }
            let client = account_client(s, &now, link.events.clone());
            match link.conns.get_mut(&s.id) {
                Some(conn) => {
                    conn.client = client;
                    conn.reach = now.reach();
                    conn.start = Some(now);
                }
                None => {
                    link.conns
                        .insert(s.id.clone(), Conn::through_account(client, now));
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

    /// The sign-in servers are dialed with now.
    pub(super) fn servers_dial_session(&self, cx: &App) -> Option<(String, String)> {
        dial_session(
            self.teams.read(cx).session(),
            crate::arakun_teams::fixture().is_some(),
        )
    }

    /// The connection a server of the account starts with, before its
    /// account was asked: held until the list is here.
    pub(super) fn servers_first_conn(
        s: &Saved,
        session: Option<&(String, String)>,
        machines: &Machines,
        events: Events,
    ) -> Conn {
        if !matches!(s.way, Way::Account { .. }) {
            return Conn::new(Client::connect(s.clone(), events));
        }
        let start = start_for(s, session, machines);
        Conn::through_account(account_client(s, &start, events), start)
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

    const LABEL: &str = "cccccccccccccccc";

    fn account_server() -> Saved {
        Saved {
            id: "s1".into(),
            name: "c".into(),
            origin: "https://app.arakun.dev".into(),
            way: Way::Account {
                label: LABEL.into(),
            },
            device: String::new(),
            token: Default::default(),
            key: String::new(),
            removed: false,
        }
    }

    /// The account's list as asked with `token`, holding the server.
    fn listed(token: &str) -> Machines {
        Machines {
            asked: signed(token),
            list: Some(vec![machine(LABEL, "server")]),
            ..Machines::default()
        }
    }

    fn dialed(token: &str, public: &str) -> Start {
        Start::Dialed(Account {
            base: "https://app.arakun.dev".into(),
            session: token.into(),
            public: public.into(),
        })
    }

    fn held_with(start: &Start) -> &str {
        match start {
            Start::Held(LinkStatus::Refused(why) | LinkStatus::Waiting { why, .. }) => why,
            Start::Held(_) => "connecting",
            Start::Dialed(_) => "dialed",
        }
    }

    #[test]
    fn a_new_list_leaves_a_server_that_is_connected_or_connecting_alone() {
        let now = dialed("tok", "key");
        for reach in [
            Reach::Connected,
            Reach::Connecting,
            Reach::Away("The connection dropped.".into()),
        ] {
            assert!(!starts_again(&now, Some(&now), Some(&reach)));
        }
        // One the list is still awaited for, or that is held: nothing to redo.
        let held = Start::Held(LinkStatus::Refused(NOT_LISTED.into()));
        assert!(!starts_again(
            &held,
            Some(&held),
            Some(&Reach::Refused(NOT_LISTED.into()))
        ));
    }

    #[test]
    fn a_new_list_dials_a_server_that_is_new_or_changed_or_gave_up() {
        let now = dialed("tok", "key");
        // No connection yet.
        assert!(starts_again(&now, None, None));
        // Held until the list came.
        let waiting = Start::Held(LinkStatus::Connecting);
        assert!(starts_again(&now, Some(&waiting), Some(&Reach::Connecting)));
        // Another sign-in, another service, another key.
        for was in [dialed("old", "key"), dialed("tok", "other")] {
            assert!(starts_again(&now, Some(&was), Some(&Reach::Connected)));
        }
        let mut elsewhere = Account {
            base: "https://other.example".into(),
            session: "tok".into(),
            public: "key".into(),
        };
        assert!(starts_again(
            &now,
            Some(&Start::Dialed(elsewhere.clone())),
            Some(&Reach::Connected)
        ));
        elsewhere.base = "https://app.arakun.dev".into();
        assert!(!starts_again(
            &now,
            Some(&Start::Dialed(elsewhere)),
            Some(&Reach::Connected)
        ));
        // Refused with the same sign-in and key: it gave up, so once more.
        assert!(starts_again(
            &now,
            Some(&now),
            Some(&Reach::Refused(
                "This server was removed from your account.".into()
            ))
        ));
        // A connection from before its start was kept.
        assert!(starts_again(&now, None, Some(&Reach::Connected)));
    }

    #[test]
    fn a_sign_out_stops_the_account_connections() {
        let s = account_server();
        let was = start_for(&s, signed("tok").as_ref(), &listed("tok"));
        assert!(matches!(was, Start::Dialed(_)));
        // Signed out: whether the list was dropped yet or not.
        for machines in [listed("tok"), Machines::default()] {
            let now = start_for(&s, None, &machines);
            assert_eq!(held_with(&now), SIGN_IN_TO_RECONNECT);
            assert!(starts_again(&now, Some(&was), Some(&Reach::Connected)));
        }
        // Another account: held until its own list is here, never dialed
        // with the list of the one before.
        let now = start_for(&s, signed("new").as_ref(), &listed("tok"));
        assert!(matches!(now, Start::Held(LinkStatus::Connecting)));
        assert!(starts_again(&now, Some(&was), Some(&Reach::Connected)));
    }

    #[test]
    fn a_server_waiting_on_the_list_says_what_it_waits_for() {
        let s = account_server();
        let session = signed("tok");
        // Asked, no answer yet.
        let mut machines = Machines {
            asked: signed("tok"),
            ..Machines::default()
        };
        let now = start_for(&s, session.as_ref(), &machines);
        assert!(matches!(now, Start::Held(LinkStatus::Connecting)));
        // The account could not be reached: never the signed-out sentence.
        machines.error = Some("offline".into());
        let now = start_for(&s, session.as_ref(), &machines);
        assert_eq!(held_with(&now), ACCOUNT_UNREACHABLE);
        assert!(matches!(now.reach(), Reach::Away(_)));
        // It answered without this server.
        machines.error = None;
        machines.list = Some(vec![machine("aaaaaaaaaaaaaaaa", "server")]);
        let now = start_for(&s, session.as_ref(), &machines);
        assert_eq!(held_with(&now), NOT_LISTED);
        // Asking again failed: the list from before still stands.
        let mut machines = listed("tok");
        machines.error = Some("offline".into());
        let now = start_for(&s, session.as_ref(), &machines);
        assert!(matches!(now, Start::Dialed(_)));
        // Added under another service: the sign-in does not go there.
        let mut other = account_server();
        other.origin = "https://other.example".into();
        let now = start_for(&other, session.as_ref(), &listed("tok"));
        assert!(held_with(&now).contains("another account server"));
    }

    #[test]
    fn a_server_that_removed_this_app_is_not_dialed_again() {
        let mut s = account_server();
        let was = start_for(&s, signed("tok").as_ref(), &listed("tok"));
        s.removed = true;
        // Whatever the account lists, and after a restart too: the mark is
        // the file's.
        let now = start_for(&s, signed("tok").as_ref(), &listed("tok"));
        assert_eq!(held_with(&now), UNPAIRED_BY_SERVER);
        assert!(starts_again(
            &now,
            Some(&was),
            Some(&Reach::Refused(UNPAIRED_BY_SERVER.into()))
        ));
        assert!(!starts_again(
            &now,
            Some(&now),
            Some(&Reach::Refused(UNPAIRED_BY_SERVER.into()))
        ));
        assert_eq!(
            held_with(&start_for(&s, None, &Machines::default())),
            UNPAIRED_BY_SERVER
        );
    }

    #[test]
    fn a_failed_list_is_asked_again_with_a_bounded_wait() {
        let mut m = Machines::default();
        let waits: Vec<u64> = (0..8).map(|_| m.next_retry().as_secs()).collect();
        assert_eq!(waits, [2, 4, 8, 16, 32, 60, 60, 60]);
        m.stop_retrying();
        assert_eq!(m.next_retry(), FIRST_RETRY);
        // Only while a server here waits on the list.
        assert!(!waits_on_list(&[]));
        let mut direct = account_server();
        direct.way = Way::Direct;
        assert!(!waits_on_list(&[direct]));
        assert!(waits_on_list(&[account_server()]));
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
            your_servers(&machines, None),
            YourServers::SignedOut
        ));
        machines.asked = Some(("https://app.arakun.dev".into(), "tok".into()));
        assert!(matches!(
            your_servers(&machines, None),
            YourServers::Loading
        ));
        let mut seen = machine("cccccccccccccccc", "server");
        seen.online = false;
        seen.last_seen_at = 1_800_000_000;
        machines.list = Some(vec![
            machine("aaaaaaaaaaaaaaaa", "server"),
            machine("bbbbbbbbbbbbbbbb", "app"),
            seen,
            machine("dddddddddddddddd", "server"),
        ]);
        let YourServers::Listed(rows) = your_servers(&machines, Some("dddddddddddddddd")) else {
            panic!("listed");
        };
        let shown: Vec<(&str, bool, u64)> = rows
            .iter()
            .map(|r| (r.label.as_str(), r.online, r.last_seen))
            .collect();
        assert_eq!(
            shown,
            [
                ("aaaaaaaaaaaaaaaa", true, 0),
                ("cccccccccccccccc", false, 1_800_000_000)
            ]
        );
        machines.error = Some("offline".into());
        assert!(matches!(
            your_servers(&machines, None),
            YourServers::Failed(_)
        ));
    }

    #[test]
    fn a_server_on_the_account_joins_the_list_by_itself() {
        let base = "https://app.arakun.dev";
        let list = vec![
            machine("aaaaaaaaaaaaaaaa", "server"),
            machine("bbbbbbbbbbbbbbbb", "app"),
            machine(LABEL, "server"),
            machine("dddddddddddddddd", "server"),
            machine("eeeeeeeeeeeeeeee", "server"),
        ];
        // One already here, and one paired by link under the same name.
        let mut by_link = account_server();
        by_link.id = "s2".into();
        by_link.way = Way::Direct;
        by_link.name = "EEEEEEEEEEEEEEEE box".into();
        let saved = vec![account_server(), by_link];
        let new = joining(&list, &saved, Some("dddddddddddddddd"), base);
        let labels: Vec<Option<&str>> = new.iter().map(|s| s.label()).collect();
        // Not this computer, not an app, not one here already in either way.
        assert_eq!(labels, [Some("aaaaaaaaaaaaaaaa")]);
        let joined = &new[0];
        assert_eq!(joined.name, "aaaaaaaaaaaaaaaa box");
        assert_eq!(joined.origin, base);
        assert!(joined.token.is_empty() && joined.device.is_empty() && !joined.removed);
        assert!(matches!(joined.way, Way::Account { .. }));
        // Once kept, it does not join again.
        let mut all = saved.clone();
        all.extend(new);
        assert!(joining(&list, &all, Some("dddddddddddddddd"), base).is_empty());
    }
}
