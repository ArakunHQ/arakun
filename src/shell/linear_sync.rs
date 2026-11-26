//! Keeping Arakun's server in step with the Linear opt-in: which Linear
//! teams and projects send issues to this computer.
//!
//! Nothing about Linear goes to the server until the switch is on. With it
//! off and no link there, a sync only asks for the links, to tell the page
//! whether Arakun's Linear app is installed. The Linear key goes only to
//! `linear_link`, only while the switch is on, and only for a workspace the
//! person connected. There is no timer: a failed
//! sync waits for the next change, relay connection or sign-in.

use std::path::PathBuf;

use gpui::{AppContext as _, Context, Window};

use super::Shell;
use crate::arakun_teams::api::DEFAULT_URL;
use crate::cloud;
use crate::linear::delegation::{Delegation, Routes, routes};
use crate::linear::{self, Session};
use crate::linear_view::{Host, Sync};
use crate::phone;

/// Where the shell stands with the server. No `Debug`: it holds the session.
#[derive(Default)]
pub(super) struct LinearSync {
    /// A change the server has not taken yet.
    owed: bool,
    running: bool,
    /// The session of the account last seen with the flag, so a sign-in
    /// syncs once and not on every change of Teams.
    account: Option<String>,
    /// The session of the Arakun account last seen, flag or not, to tell
    /// when the installs the page learned belong to another account.
    seen: Option<String>,
}

/// What one run learned, and the call that stopped it, if one did.
#[derive(Default)]
pub struct Ran {
    /// Per Linear organization id, whether Arakun's Linear app is installed.
    pub installed: Vec<(String, bool)>,
    /// The server's links were read. A run that failed before that knows
    /// nothing, and the page keeps what it had.
    pub learned: bool,
    pub error: Option<cloud::Error>,
    /// The switch went off during the run, so it stopped before sending
    /// the key or routes. Not a failure: the next run sends the empty routes.
    pub stopped: bool,
}

/// The signed-in Linear workspaces by organization id, each with the sign-in
/// that links it. Two sign-ins to one workspace link with the first. No key
/// is read here: reading the keyring can prompt the person, so a key is read
/// only for a workspace that gets linked.
pub fn keys(sessions: &[Session]) -> Vec<(String, Session)> {
    let mut keys: Vec<(String, Session)> = Vec::new();
    for s in sessions {
        if keys.iter().any(|(w, _)| *w == s.workspace_id) {
            continue;
        }
        // The same rule as reading a delegated issue, so both act as one person.
        let Some(first) = linear::session_for(sessions, &s.workspace_id) else {
            continue;
        };
        keys.push((first.workspace_id.clone(), first.clone()));
    }
    keys
}

/// A sign-in whose Linear key can be read, when a link needs it.
pub trait Key {
    /// The key, or why it could not be read. The reason never holds the key.
    fn read(&self) -> Result<String, String>;
}

impl Key for Session {
    fn read(&self) -> Result<String, String> {
        linear::key_for(self).map_err(|e| match e {
            linear::ApiError::SignedOut => "no Linear key for that workspace".into(),
            e => e.message(),
        })
    }
}

/// One sync, call by call. It blocks, so it runs off the UI thread. `keys`
/// comes from [`keys`]; a key is read only for a `Link` step and goes only to
/// `linear_link`. `still_on` reads
/// the switch again before a link or a non-empty route: `d` is from when the
/// run began, and the person may have turned it off since.
pub fn run(
    base: &str,
    session: &str,
    label: &str,
    d: &Delegation,
    keys: &[(String, impl Key)],
    local: &[PathBuf],
    still_on: impl Fn() -> bool,
) -> Ran {
    let mut ran = Ran::default();
    let links = match cloud::linear_links(base, session) {
        Ok(links) => links,
        Err(e) => {
            ran.error = Some(e);
            return ran;
        }
    };
    ran.learned = true;
    ran.installed = links
        .iter()
        .map(|l| (l.organization_id.clone(), l.installed))
        .collect();
    let workspaces: Vec<(String, bool)> = keys
        .iter()
        .map(|(w, _)| (w.clone(), links.iter().any(|l| l.organization_id == *w)))
        .collect();
    // A workspace whose call was refused: the rest of its calls are skipped,
    // and the other workspaces still go.
    let mut refused: Option<String> = None;
    let linked: Vec<String> = links.iter().map(|l| l.organization_id.clone()).collect();
    let mut steps = plan(d, &workspaces, local);
    steps.extend(clear_signed_out(&workspaces, &linked));
    for step in steps {
        if refused.as_deref() == Some(step.workspace()) {
            continue;
        }
        let sends = match &step {
            Step::Link { .. } => true,
            Step::Routes { routes, .. } => !routes.teams.is_empty() || !routes.projects.is_empty(),
        };
        if sends && !still_on() {
            ran.stopped = true;
            break;
        }
        let done = match &step {
            Step::Link { workspace } => {
                // Every planned workspace came from `keys`; without its key
                // the routes after it would only be refused.
                let Some((_, sign_in)) = keys.iter().find(|(w, _)| w == workspace) else {
                    ran.error = Some(cloud::Error::Refused(
                        "no Linear sign-in for that workspace".into(),
                    ));
                    break;
                };
                // A key the keyring won't give fails this workspace only, like
                // a refused one: the others don't need it.
                match sign_in.read() {
                    Ok(key) => cloud::linear_link(base, session, &key)
                        .map(|l| ran.installed.push((l.organization_id, l.installed))),
                    Err(reason) => {
                        refused = Some(workspace.clone());
                        ran.error = Some(cloud::Error::Refused(reason));
                        continue;
                    }
                }
            }
            Step::Routes { workspace, routes } => cloud::linear_put_routes(
                base,
                session,
                label,
                workspace,
                &routes.teams,
                &routes.projects,
            ),
        };
        match done {
            Ok(()) => {}
            // A refusal is about this workspace (a bad key, a link taken
            // over). A limit, a sign-out or no network would hit the
            // next one too.
            Err(e @ cloud::Error::Status { status, .. }) if status != 429 => {
                refused = Some(step.workspace().to_string());
                ran.error = Some(e);
            }
            Err(e) => {
                ran.error = Some(e);
                break;
            }
        }
    }
    ran
}

/// What a sync needs: the Arakun account's server and session, and this
/// computer's relay label. No `Debug`: it holds the session.
pub(super) struct Gate {
    pub base: String,
    pub session: String,
    pub label: String,
}

/// Whether a sync may run: only signed in, with the `linear` flag, and with
/// the relay set up. Otherwise nothing about Linear is sent.
pub(super) fn may_sync(
    account: Option<crate::arakun_teams::Account>,
    flagged: bool,
    label: Option<String>,
) -> Option<Gate> {
    let account = account.filter(|_| flagged)?;
    Some(Gate {
        base: account.base,
        session: account.session,
        label: label?,
    })
}

impl Shell {
    /// What the Linear page needs to know of this window.
    pub(super) fn linear_host(&self, cx: &Context<Self>) -> Host {
        let projects = self
            .projects
            .iter()
            .filter(|p| !p.is_remote())
            .map(|p| (p.name.to_string(), p.path.clone()))
            .collect();
        // A test Arakun with a Linear fixture cannot sign in to an account.
        if linear::fixture::active() {
            return Host {
                projects,
                ..Host::for_fixture()
            };
        }
        let teams = self.teams.read(cx);
        let account = teams.account();
        let base = account.as_ref().map_or(DEFAULT_URL, |a| a.base.as_str());
        Host {
            signed_in: account.is_some(),
            flagged: account.is_some() && teams.linear_enabled(),
            relay_on: self.relay_on(),
            projects,
            install_url: format!("{}/linear/install", base.trim_end_matches('/')),
        }
    }

    pub(super) fn push_linear_host(&self, cx: &mut Context<Self>) {
        let host = self.linear_host(cx);
        self.views.linear.update(cx, |v, cx| v.set_host(host, cx));
    }

    /// Teams changed: the page hears of it, and an account that may now use
    /// Linear (a sign-in, or the flag) is synced once.
    pub(super) fn linear_account_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.push_linear_host(cx);
        let teams = self.teams.read(cx);
        // Another account, or none: what the last one's links said is stale.
        let seen = teams.account().map(|a| a.session);
        if seen != self.linear_sync.seen {
            self.linear_sync.seen = seen;
            self.views
                .linear
                .update(cx, |v, cx| v.set_installed(&[], true, cx));
        }
        let teams = self.teams.read(cx);
        let now = teams
            .account()
            .filter(|_| teams.linear_enabled())
            .map(|a| a.session);
        if now != self.linear_sync.account {
            self.linear_sync.account = now.clone();
            if now.is_some() {
                self.sync_linear(window, cx);
            }
        }
    }

    /// Brings the server in line with the switch and the mappings. A call
    /// while one runs is owed, and makes one more run after it.
    pub(super) fn sync_linear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A test Arakun's account is made up: the server would sign it out.
        if linear::fixture::active() || crate::arakun_teams::fixture().is_some() {
            return;
        }
        let teams = self.teams.read(cx);
        let gate = may_sync(
            teams.account(),
            teams.linear_enabled(),
            phone::Settings::load().relay_label(),
        );
        let Some(Gate {
            base,
            session,
            label,
        }) = gate
        else {
            // Nothing was checked, so the page must not say it is up to date.
            self.views
                .linear
                .update(cx, |v, cx| v.set_sync(Sync::Unknown, cx));
            return;
        };
        self.linear_sync.owed = true;
        if self.linear_sync.running {
            return;
        }
        self.linear_sync.running = true;
        self.linear_sync.owed = false;
        self.views
            .linear
            .update(cx, |v, cx| v.set_sync(Sync::Sending, cx));
        let d = Delegation::load();
        let keys = keys(&linear::sessions());
        let local: Vec<PathBuf> = self
            .projects
            .iter()
            .filter(|p| !p.is_remote())
            .map(|p| p.path.clone())
            .collect();
        let token = session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let ran = cx
                .background_spawn(async move {
                    run(&base, &token, &label, &d, &keys, &local, || {
                        Delegation::load().enabled
                    })
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.linear_synced(session, ran, window, cx)
            });
        })
        .detach();
    }

    fn linear_synced(
        &mut self,
        session: String,
        ran: Ran,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.linear_sync.running = false;
        // Sent with an account that is no longer the one signed in: what
        // it said is not this one's, and the change is still owed.
        if self.teams.read(cx).account().map(|a| a.session) != Some(session) {
            self.linear_sync.owed = true;
            self.views
                .linear
                .update(cx, |v, cx| v.set_sync(Sync::Unknown, cx));
            self.sync_linear(window, cx);
            return;
        }
        if ran.learned {
            self.views
                .linear
                .update(cx, |v, cx| v.set_installed(&ran.installed, false, cx));
        }
        if ran.stopped {
            self.linear_sync.owed = true;
        }
        match ran.error {
            None => {
                self.views
                    .linear
                    .update(cx, |v, cx| v.set_sync(Sync::Idle, cx));
                if self.linear_sync.owed {
                    self.sync_linear(window, cx);
                }
            }
            Some(e) => {
                self.linear_sync.owed = true;
                let sync = outcome(&e);
                self.views.linear.update(cx, |v, cx| v.set_sync(sync, cx));
                if e == cloud::Error::SignedOut {
                    self.account_call_failed(e, cx);
                }
            }
        }
    }
}

/// One call to the server, in the order they go.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Link the Linear account of `workspace` (an organization id).
    Link { workspace: String },
    /// Set what `workspace` sends this computer.
    Routes { workspace: String, routes: Routes },
}

impl Step {
    pub fn workspace(&self) -> &str {
        match self {
            Step::Link { workspace } | Step::Routes { workspace, .. } => workspace,
        }
    }
}

/// The calls that bring the server in line with `d`. `workspaces` holds each
/// signed-in Linear workspace's organization id and whether the server has a
/// link for it. Only the switch links, only a workspace the person agreed to
/// send the key of, and only when there is something to send: off never
/// links. A linked workspace always gets its routes, empty when off, so the
/// server stops sending what this computer no longer takes; that sends no key.
pub fn plan(d: &Delegation, workspaces: &[(String, bool)], local: &[PathBuf]) -> Vec<Step> {
    let mut steps = Vec::new();
    for (workspace, linked) in workspaces {
        let routes = routes(d, workspace, local);
        let sendable = !routes.teams.is_empty() || !routes.projects.is_empty();
        // Unlinked and not agreed to: its routes would only be refused. A
        // consented workspace whose link went (deleted or taken over on the
        // server) is linked again, since the person agreed for it.
        if !linked && !(sendable && d.consents(workspace)) {
            continue;
        }
        if !linked {
            steps.push(Step::Link {
                workspace: workspace.clone(),
            });
        }
        steps.push(Step::Routes {
            workspace: workspace.clone(),
            routes,
        });
    }
    steps
}

/// Empty routes for each workspace the server has a link for and this
/// computer has no sign-in to. Without them the server would keep sending
/// that workspace's issues here after the person signed out of it. Never a
/// link: there is no key. The server's answer holds nothing about routes, so
/// this goes on every sync while the link stays, to keep "off" true.
pub fn clear_signed_out(workspaces: &[(String, bool)], linked: &[String]) -> Vec<Step> {
    linked
        .iter()
        .filter(|l| !workspaces.iter().any(|(w, _)| w == *l))
        .map(|l| Step::Routes {
            workspace: l.clone(),
            routes: Routes::default(),
        })
        .collect()
}

/// How a failed sync reads on the page: a limit passes, a failure waits for
/// the next reconnect.
pub fn outcome(e: &cloud::Error) -> Sync {
    match e {
        cloud::Error::Status { status: 429, .. } => Sync::Limited,
        _ => Sync::Failed,
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::linear::delegation::{Kind, Mapping};

    fn mapping(workspace: &str, kind: Kind, target: &str, path: &str) -> Mapping {
        Mapping {
            workspace: workspace.into(),
            kind,
            target: target.into(),
            name: target.into(),
            project: p(path),
        }
    }
    fn team(workspace: &str, target: &str, path: &str) -> Mapping {
        mapping(workspace, Kind::Team, target, path)
    }
    fn project(workspace: &str, target: &str, path: &str) -> Mapping {
        mapping(workspace, Kind::Project, target, path)
    }
    /// On, with consent for every workspace it maps.
    fn on(mappings: Vec<Mapping>) -> Delegation {
        let mut d = on_for_only(mappings, &[]);
        for w in d
            .mappings
            .iter()
            .map(|m| m.workspace.clone())
            .collect::<Vec<_>>()
        {
            d.consent(&w);
        }
        d
    }
    /// On, with consent for `consented` only.
    fn on_for_only(mappings: Vec<Mapping>, consented: &[&str]) -> Delegation {
        Delegation {
            enabled: true,
            mappings,
            consented: consented.iter().map(|w| w.to_string()).collect(),
        }
    }
    fn off(mappings: Vec<Mapping>) -> Delegation {
        Delegation {
            enabled: false,
            mappings,
            consented: vec![],
        }
    }
    fn p(path: &str) -> PathBuf {
        PathBuf::from(path)
    }
    fn teams(ids: &[&str]) -> Routes {
        Routes {
            teams: ids.iter().map(|s| s.to_string()).collect(),
            projects: vec![],
        }
    }

    #[test]
    fn turning_on_links_then_sends_routes() {
        let d = on(vec![team("w1", "t1", "/p/a")]);
        assert_eq!(
            plan(&d, &[("w1".into(), false)], &[p("/p/a")]),
            [
                Step::Link {
                    workspace: "w1".into()
                },
                Step::Routes {
                    workspace: "w1".into(),
                    routes: teams(&["t1"])
                }
            ]
        );
    }

    #[test]
    fn a_linked_workspace_is_not_linked_again() {
        let d = on(vec![team("w1", "t1", "/p/a")]);
        assert_eq!(
            plan(&d, &[("w1".into(), true)], &[p("/p/a")]),
            [Step::Routes {
                workspace: "w1".into(),
                routes: teams(&["t1"])
            }]
        );
    }

    #[test]
    fn off_sends_empty_routes_to_linked_workspaces_and_never_links() {
        let d = off(vec![team("w1", "t1", "/p/a")]);
        assert_eq!(
            plan(
                &d,
                &[("w1".into(), true), ("w2".into(), false)],
                &[p("/p/a")]
            ),
            [Step::Routes {
                workspace: "w1".into(),
                routes: Routes::default()
            }]
        );
    }

    #[test]
    fn each_workspace_gets_only_its_own_targets() {
        let d = on(vec![team("w1", "t1", "/p/a"), project("w2", "x2", "/p/b")]);
        assert_eq!(
            plan(
                &d,
                &[("w1".into(), true), ("w2".into(), true)],
                &[p("/p/a"), p("/p/b")]
            ),
            [
                Step::Routes {
                    workspace: "w1".into(),
                    routes: teams(&["t1"])
                },
                Step::Routes {
                    workspace: "w2".into(),
                    routes: Routes {
                        teams: vec![],
                        projects: vec!["x2".into()]
                    }
                }
            ]
        );
    }

    #[test]
    fn a_broken_mapping_is_left_out_and_the_rest_still_goes() {
        let d = on(vec![team("w1", "t1", "/p/a"), team("w1", "t2", "/p/gone")]);
        assert_eq!(
            plan(&d, &[("w1".into(), true)], &[p("/p/a")]),
            [Step::Routes {
                workspace: "w1".into(),
                routes: teams(&["t1"])
            }]
        );
    }

    #[test]
    fn nothing_sendable_and_no_link_is_no_step() {
        let d = on(vec![team("w1", "t1", "/p/gone")]);
        assert_eq!(plan(&d, &[("w1".into(), false)], &[p("/p/a")]), []);
        // Linked, the same leaves an empty list on the server.
        assert_eq!(
            plan(&d, &[("w1".into(), true)], &[p("/p/a")]),
            [Step::Routes {
                workspace: "w1".into(),
                routes: Routes::default()
            }]
        );
    }

    #[test]
    fn a_second_workspace_is_not_linked_until_consented() {
        // On for w1; then w2 is signed in to and mapped, with no consent.
        let mut d = on_for_only(
            vec![team("w1", "t1", "/p/a"), team("w2", "t2", "/p/a")],
            &["w1"],
        );
        let ws = [("w1".to_string(), true), ("w2".to_string(), false)];
        assert_eq!(
            plan(&d, &ws, &[p("/p/a")]),
            [Step::Routes {
                workspace: "w1".into(),
                routes: teams(&["t1"])
            }]
        );
        // Connect pressed for w2.
        d.consent("w2");
        assert_eq!(
            plan(&d, &ws, &[p("/p/a")])[1..],
            [
                Step::Link {
                    workspace: "w2".into()
                },
                Step::Routes {
                    workspace: "w2".into(),
                    routes: teams(&["t2"])
                }
            ]
        );
    }

    #[test]
    fn turning_on_with_two_mapped_workspaces_links_only_the_consented_one() {
        let d = on_for_only(
            vec![team("w1", "t1", "/p/a"), team("w2", "t2", "/p/a")],
            &["w2"],
        );
        let steps = plan(
            &d,
            &[("w1".into(), false), ("w2".into(), false)],
            &[p("/p/a")],
        );
        let links: Vec<&Step> = steps
            .iter()
            .filter(|s| matches!(s, Step::Link { .. }))
            .collect();
        assert_eq!(
            links,
            [&Step::Link {
                workspace: "w2".into()
            }]
        );
        // w1 is neither linked nor consented: its routes would be refused.
        assert!(steps.iter().all(|s| matches!(
            s,
            Step::Link { workspace } | Step::Routes { workspace, .. } if workspace == "w2"
        )));
    }

    #[test]
    fn a_consented_workspace_whose_link_vanished_is_linked_again() {
        // The link was deleted or taken over on the server. The person
        // agreed for this workspace, so it is linked again without asking.
        let d = on(vec![team("w1", "t1", "/p/a")]);
        assert_eq!(
            plan(&d, &[("w1".into(), false)], &[p("/p/a")])[0],
            Step::Link {
                workspace: "w1".into()
            }
        );
    }

    #[test]
    fn a_linked_workspace_gets_its_routes_without_consent() {
        // No key goes for a linked workspace, so there is nothing to agree to.
        let d = on_for_only(vec![team("w1", "t1", "/p/a")], &[]);
        assert_eq!(
            plan(&d, &[("w1".into(), true)], &[p("/p/a")]),
            [Step::Routes {
                workspace: "w1".into(),
                routes: teams(&["t1"])
            }]
        );
    }

    #[test]
    fn a_workspace_signed_out_here_gets_empty_routes_and_never_a_link() {
        // The server has links for w1 (signed in) and w9 (signed out here).
        let ws = [("w1".to_string(), true)];
        let links = ["w1".to_string(), "w9".to_string()];
        assert_eq!(
            clear_signed_out(&ws, &links),
            [Step::Routes {
                workspace: "w9".into(),
                routes: Routes::default()
            }]
        );
        // Signed in but not linked is `plan`'s business, not this one's.
        assert_eq!(clear_signed_out(&[("w2".into(), false)], &[]), []);
    }

    use std::io::{Read as _, Write as _};

    /// A server of our own that answers each request with the next of
    /// `answers`, then stops listening. Hands back every request it got.
    /// `linear_run`'s tests use it too.
    pub(in crate::shell) fn serve(
        answers: Vec<(u16, &'static str)>,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in answers {
                let (mut s, _) = listener.accept().unwrap();
                let mut got = Vec::new();
                let mut buf = [0u8; 4096];
                let head_end = loop {
                    let n = s.read(&mut buf).unwrap();
                    got.extend_from_slice(&buf[..n]);
                    if let Some(i) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&got[..head_end]).to_lowercase();
                let want: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .map_or(0, |v| v.trim().parse().unwrap());
                while got.len() < head_end + want {
                    let n = s.read(&mut buf).unwrap();
                    got.extend_from_slice(&buf[..n]);
                }
                tx.send(String::from_utf8_lossy(&got).to_string()).unwrap();
                write!(
                    s,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (base, rx)
    }

    /// A sign-in with a key in a fake keyring, counting the times it is read.
    struct Fake {
        key: Result<String, String>,
        reads: std::cell::Cell<usize>,
    }

    impl Key for Fake {
        fn read(&self) -> Result<String, String> {
            self.reads.set(self.reads.get() + 1);
            self.key.clone()
        }
    }

    fn key(workspace: &str) -> (String, Fake) {
        let key = Ok(format!("lin_api_{workspace}"));
        let reads = Default::default();
        (workspace.into(), Fake { key, reads })
    }

    /// A sign-in whose key the keyring won't give (locked, or the prompt
    /// was dismissed).
    fn locked(workspace: &str) -> (String, Fake) {
        let key = Err("The keyring could not give the Linear key: locked".into());
        let reads = Default::default();
        (workspace.into(), Fake { key, reads })
    }

    #[test]
    fn off_and_unlinked_asks_once_and_sends_nothing() {
        let (base, rx) = serve(vec![(200, r#"{"links": []}"#)]);
        let d = off(vec![team("w1", "t1", "/p/a")]);
        let ran = run(&base, "s", "k1", &d, &[key("w1")], &[p("/p/a")], || true);
        assert!(ran.error.is_none() && ran.installed.is_empty());
        let got: Vec<String> = rx.try_iter().collect();
        assert_eq!(got.len(), 1);
        assert!(got[0].starts_with("GET /linear/link "), "{}", got[0]);
        assert!(!got[0].contains("lin_api_"));
    }

    #[test]
    fn on_links_with_the_key_then_sends_routes_without_it() {
        let (base, rx) = serve(vec![
            (200, r#"{"links": []}"#),
            (
                200,
                r#"{"organization_id": "w1", "linear_user_id": "u1", "installed": false}"#,
            ),
            (200, r#"{"ok": true}"#),
        ]);
        let d = on(vec![team("w1", "t1", "/p/a")]);
        let ran = run(&base, "s", "k1", &d, &[key("w1")], &[p("/p/a")], || true);
        assert!(ran.error.is_none());
        assert_eq!(ran.installed, [("w1".to_string(), false)]);
        let got: Vec<String> = rx.try_iter().collect();
        assert_eq!(got.len(), 3);
        assert!(got[0].starts_with("GET /linear/link ") && !got[0].contains("lin_api_"));
        assert!(got[1].starts_with("POST /linear/link ") && got[1].contains("lin_api_w1"));
        assert!(got[2].starts_with("PUT /linear/routes ") && !got[2].contains("lin_api_"));
        assert!(got[2].contains(r#""teams":["t1"]"#), "{}", got[2]);
    }

    #[test]
    fn an_unconsented_workspace_key_never_leaves() {
        let (base, rx) = serve(vec![
            (200, r#"{"links": []}"#),
            (
                200,
                r#"{"organization_id": "w1", "linear_user_id": "u1", "installed": true}"#,
            ),
            (200, r#"{"ok": true}"#),
            // Room for the calls a wrong run would make for w2.
            (
                200,
                r#"{"organization_id": "w2", "linear_user_id": "u2", "installed": true}"#,
            ),
            (200, r#"{"ok": true}"#),
        ]);
        let d = on_for_only(
            vec![team("w1", "t1", "/p/a"), team("w2", "t2", "/p/a")],
            &["w1"],
        );
        let ran = run(
            &base,
            "s",
            "k1",
            &d,
            &[key("w1"), key("w2")],
            &[p("/p/a")],
            || true,
        );
        assert!(ran.error.is_none());
        let got: Vec<String> = rx.try_iter().collect();
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|r| !r.contains("lin_api_w2")), "{got:?}");
        assert!(got[1].contains("lin_api_w1"));
    }

    #[test]
    fn one_workspace_refused_does_not_stop_the_next() {
        let (base, rx) = serve(vec![
            (
                200,
                r#"{"links": [{"organization_id": "w2", "linear_user_id": "u2", "installed": true}]}"#,
            ),
            (400, r#"{"error": "bad key"}"#),
            (200, r#"{"ok": true}"#),
        ]);
        let d = on(vec![team("w1", "t1", "/p/a"), team("w2", "t2", "/p/a")]);
        let ran = run(
            &base,
            "s",
            "k1",
            &d,
            &[key("w1"), key("w2")],
            &[p("/p/a")],
            || true,
        );
        assert_eq!(ran.error.as_ref().map(outcome), Some(Sync::Failed));
        let got: Vec<String> = rx.try_iter().collect();
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got[1].starts_with("POST /linear/link ") && got[1].contains("lin_api_w1"));
        // w1's routes are skipped; w2's still go.
        assert!(got[2].starts_with("PUT /linear/routes "), "{}", got[2]);
        assert!(got[2].contains(r#""teams":["t2"]"#), "{}", got[2]);
    }

    /// A switch that reads on `n` times, then off.
    fn on_for(n: usize) -> impl Fn() -> bool {
        let left = std::cell::Cell::new(n);
        move || {
            let n = left.get();
            left.set(n.saturating_sub(1));
            n > 0
        }
    }

    #[test]
    fn turned_off_before_the_link_the_key_stays_here() {
        let (base, rx) = serve(vec![(200, r#"{"links": []}"#)]);
        let d = on(vec![team("w1", "t1", "/p/a")]);
        let ran = run(&base, "s", "k1", &d, &[key("w1")], &[p("/p/a")], on_for(0));
        assert!(ran.stopped && ran.error.is_none());
        let got: Vec<String> = rx.try_iter().collect();
        assert_eq!(got.len(), 1);
        assert!(got[0].starts_with("GET /linear/link "));
        assert!(got.iter().all(|r| !r.contains("lin_api_")));
    }

    #[test]
    fn turned_off_after_the_link_no_routes_go() {
        let (base, rx) = serve(vec![
            (200, r#"{"links": []}"#),
            (
                200,
                r#"{"organization_id": "w1", "linear_user_id": "u1", "installed": true}"#,
            ),
        ]);
        let d = on(vec![team("w1", "t1", "/p/a")]);
        let ran = run(&base, "s", "k1", &d, &[key("w1")], &[p("/p/a")], on_for(1));
        assert!(ran.stopped && ran.error.is_none());
        assert_eq!(ran.installed, [("w1".to_string(), true)]);
        let got: Vec<String> = rx.try_iter().collect();
        assert_eq!(got.len(), 2);
        assert!(got[1].starts_with("POST /linear/link "));
        assert!(!got.iter().any(|r| r.starts_with("PUT ")));
    }

    #[test]
    fn a_failed_call_stops_the_run() {
        let (base, rx) = serve(vec![
            (
                200,
                r#"{"links": [{"organization_id": "w1", "linear_user_id": "u1", "installed": true}]}"#,
            ),
            (429, r#"{"error": "too many requests"}"#),
        ]);
        let d = on(vec![team("w1", "t1", "/p/a"), team("w2", "t2", "/p/a")]);
        let ran = run(
            &base,
            "s",
            "k1",
            &d,
            &[key("w1"), key("w2")],
            &[p("/p/a")],
            || true,
        );
        assert_eq!(ran.installed, [("w1".to_string(), true)]);
        assert_eq!(ran.error.as_ref().map(outcome), Some(Sync::Limited));
        assert_eq!(rx.try_iter().count(), 2);
    }

    #[test]
    fn the_first_sign_in_to_a_workspace_holds_its_key() {
        let session = |workspace: &str, user: &str| Session {
            id: format!("{workspace}#{user}"),
            user_id: user.into(),
            name: String::new(),
            email: String::new(),
            workspace_id: workspace.into(),
            workspace: workspace.into(),
            url_key: String::new(),
            // The key is in the keyring; the file keeps none.
            credential: linear::Credential::ApiKey { key: String::new() },
            added_at: 0,
        };
        let picked: Vec<(String, String)> =
            keys(&[session("w1", "a"), session("w2", "b"), session("w1", "c")])
                .into_iter()
                .map(|(w, s)| (w, s.id))
                .collect();
        assert_eq!(
            picked,
            [
                ("w1".to_string(), "w1#a".to_string()),
                ("w2".to_string(), "w2#b".to_string())
            ]
        );
    }

    #[test]
    fn a_workspace_signed_out_here_is_cleared_on_the_server_without_a_key() {
        let (base, rx) = serve(vec![
            (
                200,
                r#"{"links": [{"organization_id": "w9", "linear_user_id": "u9", "installed": true}]}"#,
            ),
            (200, r#"{"ok": true}"#),
        ]);
        // On and mapped, but w9 has no sign-in here: no key can exist for it.
        let d = on(vec![team("w9", "t9", "/p/a")]);
        let no_keys: [(String, Fake); 0] = [];
        let ran = run(&base, "s", "k1", &d, &no_keys, &[p("/p/a")], || true);
        assert!(ran.error.is_none());
        let got: Vec<String> = rx.try_iter().collect();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].starts_with("GET /linear/link "));
        assert!(got[1].starts_with("PUT /linear/routes "), "{}", got[1]);
        assert!(got[1].contains(r#""teams":[]"#), "{}", got[1]);
        assert!(got[1].contains(r#""projects":[]"#), "{}", got[1]);
        assert!(got.iter().all(|r| !r.contains("lin_api_")), "{got:?}");
    }

    #[test]
    fn off_still_clears_a_signed_out_workspace_once() {
        let (base, rx) = serve(vec![
            (
                200,
                r#"{"links": [{"organization_id": "w9", "linear_user_id": "u9", "installed": true}]}"#,
            ),
            (200, r#"{"ok": true}"#),
        ]);
        let no_keys: [(String, Fake); 0] = [];
        let ran = run(&base, "s", "k1", &off(vec![]), &no_keys, &[], || false);
        assert!(ran.error.is_none() && !ran.stopped);
        let got: Vec<String> = rx.try_iter().collect();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[1].starts_with("PUT /linear/routes "), "{}", got[1]);
    }

    #[test]
    fn a_run_that_failed_before_the_links_learned_nothing() {
        let (base, _rx) = serve(vec![(500, r#"{"error": "down"}"#)]);
        let ran = run(&base, "s", "k1", &off(vec![]), &[key("w1")], &[], || true);
        assert!(ran.error.is_some() && !ran.learned && ran.installed.is_empty());
        let (base, _rx) = serve(vec![(200, r#"{"links": []}"#)]);
        let ran = run(&base, "s", "k1", &off(vec![]), &[key("w1")], &[], || true);
        assert!(ran.learned);
    }

    #[test]
    fn a_key_is_read_only_for_a_workspace_that_gets_linked() {
        let (base, _rx) = serve(vec![
            (
                200,
                r#"{"links": [{"organization_id": "w1", "linear_user_id": "u1", "installed": true}]}"#,
            ),
            (200, r#"{"ok": true}"#),
            (
                200,
                r#"{"organization_id": "w2", "linear_user_id": "u2", "installed": true}"#,
            ),
            (200, r#"{"ok": true}"#),
        ]);
        // w1 is linked, w2 gets linked, w3 is not consented.
        let d = on_for_only(
            vec![
                team("w1", "t1", "/p/a"),
                team("w2", "t2", "/p/a"),
                team("w3", "t3", "/p/a"),
            ],
            &["w1", "w2"],
        );
        let ks = [key("w1"), key("w2"), key("w3")];
        let ran = run(&base, "s", "k1", &d, &ks, &[p("/p/a")], || true);
        assert!(ran.error.is_none());
        let reads: Vec<usize> = ks.iter().map(|(_, k)| k.reads.get()).collect();
        assert_eq!(reads, [0, 1, 0]);
        // Off, nothing is linked, so no key is read.
        let (base, _rx) = serve(vec![(200, r#"{"links": []}"#)]);
        let ks = [key("w1")];
        run(&base, "s", "k1", &off(vec![]), &ks, &[], || true);
        assert_eq!(ks[0].1.reads.get(), 0);
    }

    #[test]
    fn a_key_the_keyring_wont_give_fails_that_workspace_only() {
        let (base, rx) = serve(vec![
            (
                200,
                r#"{"links": [{"organization_id": "w2", "linear_user_id": "u2", "installed": true}]}"#,
            ),
            (200, r#"{"ok": true}"#),
        ]);
        let d = on(vec![team("w1", "t1", "/p/a"), team("w2", "t2", "/p/a")]);
        let ran = run(
            &base,
            "s",
            "k1",
            &d,
            &[locked("w1"), key("w2")],
            &[p("/p/a")],
            || true,
        );
        assert_eq!(ran.error.as_ref().map(outcome), Some(Sync::Failed));
        let got: Vec<String> = rx.try_iter().collect();
        // No link for w1, and none of its routes; w2's still go.
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].starts_with("GET /linear/link "));
        assert!(got[1].starts_with("PUT /linear/routes "), "{}", got[1]);
        assert!(got[1].contains(r#""teams":["t2"]"#), "{}", got[1]);
    }

    #[test]
    fn a_sync_needs_the_account_the_flag_and_the_label() {
        let account = || {
            Some(crate::arakun_teams::Account {
                base: "https://a.dev".into(),
                session: "s".into(),
                github_id: 1,
                login: "rik".into(),
            })
        };
        let label = || Some("k1".to_string());
        assert!(may_sync(None, true, label()).is_none()); // signed out
        assert!(may_sync(account(), false, label()).is_none()); // no flag
        assert!(may_sync(account(), true, None).is_none()); // no relay label
        let gate = may_sync(account(), true, label()).unwrap();
        assert_eq!(
            (
                gate.base.as_str(),
                gate.session.as_str(),
                gate.label.as_str()
            ),
            ("https://a.dev", "s", "k1")
        );
    }

    #[test]
    fn a_limit_reads_differently_from_a_failure() {
        let status = |status| cloud::Error::Status {
            status,
            message: String::new(),
        };
        assert_eq!(outcome(&status(429)), Sync::Limited);
        for e in [
            cloud::Error::Unreachable("offline".into()),
            cloud::Error::Refused("HTTP 500".into()),
            status(409),
            status(404),
            status(403),
            cloud::Error::SignedOut,
        ] {
            assert_eq!(outcome(&e), Sync::Failed, "{e:?}");
        }
    }
}
