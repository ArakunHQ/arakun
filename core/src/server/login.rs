//! The server's sign-in to its account, which the relay asks for. `login`
//! runs GitHub's device flow in the terminal and keeps the session in
//! `account.json` in the server's folder, next to its relay key; `logout`
//! ends it. A running server is told either way and dials the relay again.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use super::local::{self, Request};
use crate::cloud;

/// The file the sign-in is kept in, in the server's folder.
pub const FILE: &str = "account.json";

/// The account the server is signed in to. No `Debug` that shows the session:
/// it opens the account, so it must never end up in a log.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignIn {
    /// The service the session is from. Another one does not know it.
    pub base: String,
    pub session: String,
    pub session_id: String,
    pub github_id: u64,
    pub login: String,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignIn")
            .field("base", &self.base)
            .field("session_id", &self.session_id)
            .field("github_id", &self.github_id)
            .field("login", &self.login)
            .finish_non_exhaustive()
    }
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The sign-in kept in `dir`, `None` when there is none. Whoever sends its
/// session checks first that it goes to `base`.
pub fn load(dir: &Path) -> Result<Option<SignIn>, String> {
    let path = path(dir);
    let bytes = match crate::durable::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    // The parser's message could quote the file, and the file holds the session.
    serde_json::from_slice(&bytes).map(Some).map_err(|_| {
        format!(
            "{} cannot be read; `{} login` writes it again",
            path.display(),
            super::name()
        )
    })
}

/// Keeps `sign_in` in `dir`, readable by this user alone.
pub fn save(dir: &Path, sign_in: &SignIn) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(sign_in).map_err(std::io::Error::other)?;
    crate::durable::write_private(&path(dir), &bytes)
}

/// Forgets the sign-in kept in `dir`. One that is not there is forgotten.
pub fn forget(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path(dir)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The request that tells a running server its sign-in changed. The server
/// reads the file again: the session itself never goes over the socket.
pub const CHANGED: &str = "account";

/// `login`: signs in with GitHub's device flow, keeps the session and tells
/// a running server. Returns the exit code.
pub fn login(force: bool) -> i32 {
    let base = match cloud::base_url() {
        Ok(base) => base,
        Err(e) => return super::fail(&e),
    };
    let dir = super::dir();
    // A file that cannot be read is written again.
    let old = load(&dir).unwrap_or(None);
    match plan(old.as_ref(), &base, force) {
        Plan::Check(old) => match checked(cloud::me(&old.base, &old.session)) {
            Checked::Good => {
                println!(
                    "Signed in as {}. `{} login --force` signs in again.",
                    old.login,
                    super::name()
                );
                return 0;
            }
            Checked::Ended => {}
            Checked::Unknown(e) => {
                return super::fail(&format!(
                    "could not check the sign-in as {} at {}. {e} It is kept; try again, or `{} login --force` signs in again.",
                    old.login,
                    old.base,
                    super::name()
                ));
            }
        },
        Plan::Replace(old) => {
            println!("Replacing the sign-in as {} at {}.", old.login, old.base)
        }
        Plan::SignIn => {}
    }
    if let Err(e) = super::daemon::make_private_dir(&dir) {
        return super::fail(&format!("{}: {e}", dir.display()));
    }
    let new = match device_flow(&base) {
        Ok(new) => new,
        Err(e) => return super::fail(&e),
    };
    if let Err(e) = save(&dir, &new) {
        // The session works but nothing keeps it: end it rather than leave it.
        let _ = cloud::logout(&new.base, &new.session);
        return super::fail(&format!("{}: {e}", path(&dir).display()));
    }
    // The session this one replaces goes only to the service that issued it.
    if let Some(old) = old.filter(|o| o.session != new.session) {
        let _ = cloud::logout(&old.base, &old.session);
    }
    tell_server();
    println!("Signed in as {}.", new.login);
    0
}

/// What `login` does with the sign-in it finds.
#[derive(Debug, PartialEq)]
enum Plan<'a> {
    /// Ask the service whether this one still works before anything else.
    Check(&'a SignIn),
    /// It is for another service: sign in, saying what it replaces.
    Replace(&'a SignIn),
    SignIn,
}

fn plan<'a>(old: Option<&'a SignIn>, base: &str, force: bool) -> Plan<'a> {
    match old {
        None => Plan::SignIn,
        Some(old) if !cloud::same_origin(&old.base, base) => Plan::Replace(old),
        Some(_) if force => Plan::SignIn,
        Some(old) => Plan::Check(old),
    }
}

/// What the service said of a kept session.
#[derive(Debug, PartialEq)]
enum Checked {
    Good,
    /// Signed out there: sign in as if there were none.
    Ended,
    /// No answer, or one that says nothing about the session: keep it.
    Unknown(cloud::Error),
}

fn checked(me: Result<cloud::Me, cloud::Error>) -> Checked {
    match me {
        Ok(_) => Checked::Good,
        Err(cloud::Error::SignedOut) => Checked::Ended,
        Err(e) => Checked::Unknown(e),
    }
}

/// GitHub's device flow in the terminal, then a session from the service at
/// `base`. The GitHub token lives only in here.
fn device_flow(base: &str) -> Result<SignIn, String> {
    let failed = |e: cloud::Error| match e {
        // Already a sentence of its own, from GitHub or the device flow.
        cloud::Error::Refused(why) => format!("could not sign in: {why}"),
        e => format!("could not sign in. {e}"),
    };
    let client_id = cloud::github_client_id(base).map_err(failed)?;
    let code = cloud::start_device_flow(&client_id).map_err(failed)?;
    println!(
        "To sign in, open {} and enter the code {}",
        code.verification_uri, code.user_code
    );
    let deadline = Instant::now() + code.expires_in;
    let token = loop {
        std::thread::sleep(code.interval);
        if Instant::now() >= deadline {
            return Err(format!(
                "the code ran out before it was entered. Run `{} login` for a new one.",
                super::name()
            ));
        }
        match cloud::poll_device_flow(&client_id, &code) {
            Ok(None) => {}
            Ok(Some(token)) => break token,
            // A network that drops for a moment leaves the code good.
            Err(cloud::Error::Unreachable(_)) => {}
            Err(e) => return Err(failed(e)),
        }
    };
    // The service takes a name of at most 100 characters.
    let name: String = crate::phone::host_name().chars().take(100).collect();
    let signed = cloud::sign_in(base, &token, &name, "server").map_err(failed)?;
    Ok(SignIn {
        base: base.to_string(),
        session: signed.session,
        session_id: signed.session_id,
        github_id: signed.github_id,
        login: signed.login,
    })
}

/// `logout`: ends the session on the service, forgets it and tells a running
/// server. Returns the exit code.
pub fn logout() -> i32 {
    let dir = super::dir();
    // A file that cannot be read is forgotten all the same.
    let kept = match load(&dir) {
        Ok(None) => {
            println!("Not signed in.");
            return 0;
        }
        Ok(kept) => kept,
        Err(_) => None,
    };
    let ended = kept.as_ref().map(|s| cloud::logout(&s.base, &s.session));
    if let Err(e) = forget(&dir) {
        return super::fail(&format!("{}: {e}", path(&dir).display()));
    }
    tell_server();
    match (kept, ended) {
        (Some(s), Some(Err(e))) if e != cloud::Error::SignedOut => {
            println!("Signed out of {} on this machine.", s.login);
            eprintln!(
                "{}: the session could not be ended on {}. {e} It still shows among the account's signed-in devices until it is signed out there.",
                super::name(),
                s.base
            );
        }
        (Some(s), _) => println!("Signed out of {}.", s.login),
        (None, _) => println!(
            "Signed out. {} could not be read, so a session may still be active on the service; end it from another signed-in device.",
            FILE
        ),
    }
    0
}

/// The sign-in kept here, or the exit code of saying there is none.
fn kept_or_fail() -> Result<SignIn, i32> {
    match load(&super::dir()) {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Err(super::fail(&format!(
            "not signed in. Run `{} login` first.",
            super::name()
        ))),
        Err(e) => Err(super::fail(&e)),
    }
}

/// What a failed call to the account says, with the command that helps.
fn account_failed(e: cloud::Error) -> i32 {
    match e {
        cloud::Error::SignedOut => super::fail(&format!(
            "the sign-in is no longer valid. Run `{} login`.",
            super::name()
        )),
        e => super::fail(&e.to_string()),
    }
}

/// This server's label on the relay, when it has a key yet.
fn own_label() -> Option<String> {
    crate::phone::use_settings_file(super::dir().join("phone.json"));
    crate::phone::Settings::read().ok().flatten()?.relay_label()
}

/// `machines`: the account's machines, from the service this server is
/// signed in to. Returns the exit code.
pub fn machines() -> i32 {
    let kept = match kept_or_fail() {
        Ok(kept) => kept,
        Err(code) => return code,
    };
    match cloud::machines(&kept.base, &kept.session) {
        Ok(list) => {
            println!(
                "{}",
                machines_text(&list, own_label().as_deref(), crate::model::unix_now())
            );
            0
        }
        Err(e) => account_failed(e),
    }
}

/// The account's machines, one a line: label, kind, whether online, name.
fn machines_text(list: &[cloud::Machine], own_label: Option<&str>, now: u64) -> String {
    if list.is_empty() {
        return "no machines in this account yet".into();
    }
    list.iter()
        .map(|m| {
            let state = if m.online {
                "online".to_string()
            } else {
                format!("seen {}", crate::account::ago(m.last_seen_at, now))
            };
            let this = if Some(m.label.as_str()) == own_label {
                "  (this server)"
            } else {
                ""
            };
            format!(
                "{}  {:<6}  {state}  {}{this}",
                printable(&m.label),
                printable(&m.kind),
                printable(&m.name)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `s` with its control characters dropped: the names come from the
/// service, and an escape sequence in one must not drive the terminal.
fn printable(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// `forget <label>`: removes one of the account's machines. Returns the exit code.
pub fn forget_machine(label: &str) -> i32 {
    let kept = match kept_or_fail() {
        Ok(kept) => kept,
        Err(code) => return code,
    };
    match cloud::remove_machine(&kept.base, &kept.session, label) {
        Ok(true) => {
            println!("{}", forgot_text(label, own_label().as_deref()));
            0
        }
        Ok(false) => super::fail(&format!(
            "the account has no machine {}; `{} machines` lists them",
            printable(label),
            super::name()
        )),
        Err(e) => account_failed(e),
    }
}

/// What `forget` says it did. This server, still signed in, comes back
/// when it next connects, so that is said too.
fn forgot_text(label: &str, own_label: Option<&str>) -> String {
    if Some(label) == own_label {
        format!(
            "Removed {label}, this server, from the account. It is listed again the next time it connects; `{} logout` keeps it out.",
            super::name()
        )
    } else {
        format!(
            "Removed {label} from the account. If it is still signed in, it is listed again the next time it connects."
        )
    }
}

/// Tells the server running here, if one is, that its sign-in changed.
fn tell_server() {
    let socket = super::socket_path();
    match local::send(&socket, &Request::new(CHANGED, &[]), local::REPLY_WAIT) {
        Ok(reply) if !reply.ok => eprintln!("{}: {}", super::name(), reply.message),
        Ok(_) => {}
        // No server: the next one reads the file.
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) => {}
        Err(e) => eprintln!(
            "{}: could not tell the running server ({e}); restart it with `{} quit` and `{} serve`",
            super::name(),
            super::name(),
            super::name()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-login-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn octocat() -> SignIn {
        SignIn {
            base: "https://app.arakun.dev".into(),
            session: "s3cret-session".into(),
            session_id: "sid1".into(),
            github_id: 583231,
            login: "octocat".into(),
        }
    }

    #[test]
    fn a_saved_sign_in_reads_back() {
        let dir = temp("roundtrip");
        assert_eq!(load(&dir), Ok(None));
        save(&dir, &octocat()).unwrap();
        assert_eq!(load(&dir), Ok(Some(octocat())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn only_its_user_can_read_the_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp("mode");
        save(&dir, &octocat()).unwrap();
        let mode = std::fs::metadata(path(&dir)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn forgetting_removes_the_file_and_is_fine_twice() {
        let dir = temp("forget");
        save(&dir, &octocat()).unwrap();
        forget(&dir).unwrap();
        assert!(!path(&dir).exists());
        assert_eq!(load(&dir), Ok(None));
        forget(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_cannot_be_read_says_so() {
        let dir = temp("bad");
        std::fs::write(path(&dir), "{ not json").unwrap();
        let e = load(&dir).unwrap_err();
        assert!(e.contains(FILE), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn login_checks_a_sign_in_here_and_replaces_one_elsewhere() {
        let me = octocat();
        let here = "https://app.arakun.dev";
        assert_eq!(plan(None, here, false), Plan::SignIn);
        assert_eq!(plan(Some(&me), here, false), Plan::Check(&me));
        assert_eq!(
            plan(Some(&me), "https://APP.arakun.dev/", false),
            Plan::Check(&me)
        );
        assert_eq!(plan(Some(&me), here, true), Plan::SignIn);
        // Another service: no --force needed, but it says what goes.
        assert_eq!(
            plan(Some(&me), "https://relay.example", false),
            Plan::Replace(&me)
        );
        assert_eq!(
            plan(Some(&me), "https://relay.example", true),
            Plan::Replace(&me)
        );
    }

    #[test]
    fn a_session_the_service_ended_is_signed_in_again_and_no_answer_keeps_it() {
        let me = cloud::Me {
            github_id: 583231,
            login: "octocat".into(),
            flags: vec![],
            plan: String::new(),
            notices: vec![],
        };
        assert_eq!(checked(Ok(me)), Checked::Good);
        assert_eq!(checked(Err(cloud::Error::SignedOut)), Checked::Ended);
        let away = cloud::Error::Unreachable("timed out".into());
        assert_eq!(checked(Err(away.clone())), Checked::Unknown(away));
    }

    fn machine(label: &str, kind: &str, online: bool) -> cloud::Machine {
        cloud::Machine {
            label: label.into(),
            public: String::new(),
            name: format!("{label} box"),
            kind: kind.into(),
            online,
            last_seen_at: 0,
        }
    }

    #[test]
    fn machines_lists_each_with_its_state_and_marks_this_server() {
        let list = [
            machine("aaaaaaaaaaaaaaaa", "app", true),
            machine("bbbbbbbbbbbbbbbb", "server", false),
        ];
        let text = machines_text(&list, Some("bbbbbbbbbbbbbbbb"), 3 * 86_400);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].starts_with("aaaaaaaaaaaaaaaa  app     online  aaaaaaaaaaaaaaaa box"));
        assert!(!lines[0].contains("this server"), "{text}");
        assert!(
            lines[1].starts_with("bbbbbbbbbbbbbbbb  server  seen "),
            "{text}"
        );
        assert!(lines[1].ends_with("(this server)"), "{text}");
        assert_eq!(
            machines_text(&[], None, 0),
            "no machines in this account yet"
        );
        // What the service names is printed without control characters.
        let mut odd = machine("cccccccccccccccc", "app", true);
        odd.name = "evil\u{1b}]0;pwned\u{7}\nbox".into();
        let text = machines_text(&[odd], None, 0);
        assert!(!text.chars().any(char::is_control), "{text:?}");
        assert!(text.contains("evil]0;pwnedbox"), "{text:?}");
    }

    #[test]
    fn forget_says_this_server_comes_back_unless_it_logs_out() {
        let own = forgot_text("aaaaaaaaaaaaaaaa", Some("aaaaaaaaaaaaaaaa"));
        assert!(
            own.contains("this server") && own.contains("logout"),
            "{own}"
        );
        let other = forgot_text("bbbbbbbbbbbbbbbb", Some("aaaaaaaaaaaaaaaa"));
        assert!(
            other.contains("Removed bbbbbbbbbbbbbbbb") && !other.contains("logout"),
            "{other}"
        );
    }

    #[test]
    fn the_session_never_shows_in_debug_output() {
        let shown = format!("{:?}", octocat());
        assert!(shown.contains("octocat"), "{shown}");
        assert!(!shown.contains("s3cret"), "{shown}");
    }
}
