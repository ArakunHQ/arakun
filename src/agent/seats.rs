//! Claude seats: the claude.ai subscriptions Claude Code sessions run on. The
//! machine's own sign-in is one; each long-lived token (`claude setup-token`)
//! saved with `arakun seat add` is another, and so is the borrowed token
//! `plan` already reads.
//!
//! A new Claude Code session starts on the seat with the most room in its
//! 5-hour window and keeps it for life: the seat's name is saved with the
//! session, and its token goes into that session's environment only, so
//! parallel sessions spread over the seats instead of draining one. There is
//! no proxy: each session is Claude Code itself, on its own token.
//!
//! Seats are opt-in: with no named seat saved, sessions start as before.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, anyhow, bail};

use super::claude_auth;
use super::plan::{self, PlanUsage, Seats};

/// The machine's own claude.ai sign-in: no token in the environment.
pub const OWN: &str = "own";
/// The token `plan` calls borrowed (`CLAUDE_CODE_OAUTH_TOKEN` in Arakun's
/// own environment).
pub const BORROWED: &str = "borrowed";
/// What Claude Code runs on instead of its sign-in.
pub const TOKEN_VAR: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// Each live session on a seat counts as this many points of its 5-hour
/// window. Windows are read every 15 minutes at most, so sessions started
/// together would otherwise all land on the seat that looked emptiest.
const LIVE_WEIGHT: i64 = 10;

/// The seat's name as people read it.
pub fn label(id: &str) -> String {
    match id {
        OWN => "Sign-in".into(),
        BORROWED => "Borrowed token".into(),
        name => name.into(),
    }
}

// ---- storage -------------------------------------------------------------

/// Where named seats keep their tokens: one file per seat, named after it.
pub fn dir() -> PathBuf {
    crate::platform::app_dir().join("seats")
}

/// The named seats' tokens, one private file each (0600 in a 0700 folder on
/// Unix; under the user's own profile on Windows), as the CLIProxyAPI key is.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Store { dir }
    }

    /// The store in Arakun's data folder.
    pub fn open() -> Self {
        Self::new(dir())
    }

    /// The saved seats' names, sorted. Files that are not a seat (a temp
    /// file left by a crash) are skipped.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| valid_name(n).is_ok())
            .collect();
        names.sort();
        names
    }

    pub fn token(&self, name: &str) -> Option<String> {
        valid_name(name).ok()?;
        std::fs::read_to_string(self.dir.join(name))
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    }

    /// Saves `token` as seat `name`, replacing one of that name.
    pub fn add(&self, name: &str, token: &str) -> Result<()> {
        valid_name(name)?;
        check_token(token)?;
        crate::durable::write_private(&self.dir.join(name), token.trim().as_bytes())?;
        Ok(())
    }

    /// Whether there was a seat to remove.
    pub fn remove(&self, name: &str) -> Result<bool> {
        valid_name(name)?;
        match std::fs::remove_file(self.dir.join(name)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

/// A name is also a file name, on every platform: lower case letters,
/// digits, `-` and `_`, at most 32. `own` and `borrowed` name the other seats.
pub fn valid_name(name: &str) -> Result<()> {
    let ok_chars = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if name.is_empty() || name.len() > 32 || !ok_chars || name.starts_with('-') {
        bail!(
            "a seat's name is 1 to 32 lower case letters, digits, - or _, and does not start with -"
        );
    }
    if name == OWN || name == BORROWED {
        bail!("\"{name}\" is taken: it names the sign-in or the borrowed token");
    }
    Ok(())
}

fn check_token(token: &str) -> Result<()> {
    let t = token.trim();
    if t.is_empty() {
        bail!("no token: paste the one `claude setup-token` printed");
    }
    if t.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("the token must be one line with no spaces");
    }
    Ok(())
}

// ---- live sessions -------------------------------------------------------

static LIVE: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

/// A live session on a seat; dropping it ends the count.
pub struct Lease {
    id: u64,
    seat: String,
}

impl Lease {
    pub fn seat(&self) -> &str {
        &self.seat
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut live) = LIVE.lock() {
            live.retain(|(id, _)| *id != self.id);
        }
    }
}

/// Counts a session as live on `seat` until the lease is dropped.
pub fn lease(seat: &str) -> Lease {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut live) = LIVE.lock() {
        live.push((id, seat.to_string()));
    }
    Lease {
        id,
        seat: seat.to_string(),
    }
}

/// How many live sessions each seat has.
pub fn live() -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    if let Ok(live) = LIVE.lock() {
        for (_, seat) in live.iter() {
            *counts.entry(seat.clone()).or_insert(0) += 1;
        }
    }
    counts
}

// ---- when seats can't work -----------------------------------------------

/// Why sessions can't get a seat each right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// ~/.claude/settings.json sets `CLAUDE_CODE_OAUTH_TOKEN` in its `env`
    /// block. Claude Code assigns that block over its own environment, so
    /// every session would run on that token whatever it was started with.
    SettingsToken,
    /// Claude Code runs on an API key, a gateway or a cloud provider, which
    /// win over any token (its label).
    Access(String),
}

impl Blocked {
    pub fn notice(&self) -> String {
        match self {
            Blocked::SettingsToken => format!(
                "Seats are off: ~/.claude/settings.json sets {TOKEN_VAR} in its env block, and \
                 Claude Code applies that over each session's own environment, so every session \
                 runs on that one token. Remove it there (save it with `arakun seat add` instead) \
                 to give each session its own seat."
            ),
            Blocked::Access(label) => format!(
                "Seats are off: Claude Code runs on {label}, which wins over a seat's token."
            ),
        }
    }
}

/// Reads files; keep it off the UI thread.
pub fn blocked() -> Option<Blocked> {
    if plan::settings_pin_a_token() {
        return Some(Blocked::SettingsToken);
    }
    claude_auth::access().map(|a| Blocked::Access(a.label()))
}

// ---- picking -------------------------------------------------------------

/// A seat a new session could start on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    /// How full its 5-hour window is, when known.
    pub five_hour: Option<u64>,
    /// Near or over its 5-hour or weekly limit.
    pub near: bool,
    /// Live sessions on it now.
    pub live: usize,
}

impl Candidate {
    fn new(id: &str, usage: Option<&PlanUsage>, live: &HashMap<String, usize>) -> Self {
        Candidate {
            id: id.to_string(),
            five_hour: usage.and_then(PlanUsage::five_hour),
            near: usage.is_some_and(PlanUsage::near_limit),
            live: live.get(id).copied().unwrap_or(0),
        }
    }

    /// Room left in the 5-hour window, less what its live sessions will use.
    fn room(&self) -> i64 {
        100 - self.five_hour.unwrap_or(0).min(100) as i64 - LIVE_WEIGHT * self.live as i64
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    pub id: String,
    /// Every seat is near its limit: this is only the least full.
    pub crowded: bool,
}

/// The seat with the most room among those not near their limit, or among
/// all when every one is. Ties go to the first.
pub fn pick(candidates: &[Candidate]) -> Option<Picked> {
    let free: Vec<&Candidate> = candidates.iter().filter(|c| !c.near).collect();
    let crowded = free.is_empty();
    let pool = if crowded {
        candidates.iter().collect()
    } else {
        free
    };
    // `max_by_key` keeps the last of equals; reversed, that is the first.
    pool.into_iter()
        .rev()
        .max_by_key(|c| c.room())
        .map(|c| Picked {
            id: c.id.clone(),
            crowded,
        })
}

/// The seats a session can start on now, in order: the sign-in, the borrowed
/// token, then the named seats. A named seat whose token never answered is
/// left out; one not read yet is in, as empty.
fn candidates(
    seats: &Seats,
    own: bool,
    borrowed: bool,
    names: &[String],
    live: &HashMap<String, usize>,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    if own {
        out.push(Candidate::new(OWN, seats.own.as_ref(), live));
    }
    if borrowed {
        out.push(Candidate::new(BORROWED, seats.borrowed.as_ref(), live));
    }
    for name in names {
        let known = seats.named.iter().find(|s| s.name == *name);
        if known.is_some_and(|s| s.usage.is_none() && s.error.is_some()) {
            continue;
        }
        out.push(Candidate::new(
            name,
            known.and_then(|s| s.usage.as_ref()),
            live,
        ));
    }
    out
}

/// The seat a session runs on, and what that means for its environment.
pub struct Assignment {
    pub seat: String,
    /// The seat's token; `None` for the sign-in, which runs with none.
    token: Option<String>,
    /// Something to tell the session: its seat is gone, or every seat is full.
    pub notice: Option<String>,
}

// By hand, so a log of it never shows the token.
impl std::fmt::Debug for Assignment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Assignment")
            .field("seat", &self.seat)
            .field("token", &self.token.as_ref().map(|_| "[token]"))
            .field("notice", &self.notice)
            .finish()
    }
}

impl Assignment {
    /// What the session's process gets: the seat's token, or none at all for
    /// the sign-in (one in Arakun's own environment would win over it).
    pub fn env(&self) -> (Vec<(String, OsString)>, Vec<String>) {
        match &self.token {
            Some(t) => (vec![(TOKEN_VAR.into(), t.into())], Vec::new()),
            None => (Vec::new(), vec![TOKEN_VAR.into()]),
        }
    }
}

/// The seat a Claude Code session starts on: `pinned`, the one saved with
/// it, while that seat is still there; else the one [`pick`] finds. `None`
/// leaves the session as it would be without seats: none are saved, or they
/// can't work ([`blocked`]). Reads files, not the network.
pub fn assign(pinned: Option<&str>) -> Option<Assignment> {
    if blocked().is_some() {
        return None;
    }
    let store = Store::open();
    let names = store.names();
    let own = plan::own_sign_in();
    let borrowed = plan::borrowed_seat_token();
    decide(
        pinned,
        &names,
        own,
        borrowed.is_some(),
        &plan::known_seats(),
        &live(),
        |id| match id {
            OWN => own.then_some(None),
            BORROWED => borrowed.clone().map(Some),
            name => store.token(name).map(Some),
        },
    )
}

/// [`assign`] without the files. `token` says whether a seat is there and
/// its token: `Some(None)` for the sign-in.
fn decide(
    pinned: Option<&str>,
    names: &[String],
    own: bool,
    borrowed: bool,
    seats: &Seats,
    live: &HashMap<String, usize>,
    token: impl Fn(&str) -> Option<Option<String>>,
) -> Option<Assignment> {
    if let Some(seat) = pinned
        && let Some(t) = token(seat)
    {
        return Some(Assignment {
            seat: seat.to_string(),
            token: t,
            notice: None,
        });
    }
    // Opt-in: without a named seat, a new session starts as it always did.
    if pinned.is_none() && names.is_empty() {
        return None;
    }
    let picked = pick(&candidates(seats, own, borrowed, names, live))?;
    let t = token(&picked.id)?;
    let on = label(&picked.id);
    let mut notice = pinned.map(|p| {
        format!(
            "Seat {} is gone, so this session now runs on {on}.",
            label(p)
        )
    });
    if picked.crowded {
        let full = format!(
            "Every Claude seat is near its limit; this session runs on {on}, which has the most room."
        );
        notice = Some(match notice {
            Some(n) => format!("{n} {full}"),
            None => full,
        });
    }
    Some(Assignment {
        seat: picked.id,
        token: t,
        notice,
    })
}

// ---- command line --------------------------------------------------------

const USAGE: &str = "usage: arakun seat add <name>   (the token from `claude setup-token` on stdin)
       arakun seat ls
       arakun seat rm <name>";

/// `arakun seat …`: the exit code.
pub fn cli(args: &[String]) -> i32 {
    let store = Store::open();
    let mut out = std::io::stdout();
    let result = match args {
        [cmd, name] if cmd == "add" => {
            let stdin = std::io::stdin();
            add(
                &store,
                name,
                || read_token(&stdin),
                plan::fetch_borrowed,
                &mut out,
            )
            .map(|usage| plan::remember_named(name, usage))
        }
        [cmd] if cmd == "ls" => {
            let seats = plan::seats();
            let rows = list(
                &seats,
                plan::own_sign_in(),
                plan::borrowed_seat_token().is_some(),
                &store.names(),
                super::rpc::unix_now(),
            );
            let mut text = rows.join("\n");
            if let Some(b) = blocked() {
                text.push_str(&format!("\n\n{}", b.notice()));
            }
            writeln!(out, "{text}").map_err(Into::into)
        }
        [cmd, name] if cmd == "rm" => match store.remove(name) {
            Ok(true) => writeln!(
                out,
                "Removed seat {name}. Sessions on it move to another seat when they start again."
            )
            .map_err(Into::into),
            Ok(false) => Err(anyhow!("there is no seat named {name}")),
            Err(e) => Err(e),
        },
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("arakun seat: {e:#}");
            1
        }
    }
}

/// `arakun seat add`: reads the token with `read`, checks it with one cheap
/// call (`check`), and saves it. The seat's windows, as the check read them.
/// Nothing it prints, and no error it returns, carries the token.
fn add(
    store: &Store,
    name: &str,
    read: impl FnOnce() -> Result<String>,
    check: impl FnOnce(&str) -> Result<PlanUsage>,
    out: &mut impl Write,
) -> Result<PlanUsage> {
    valid_name(name)?;
    if store.token(name).is_some() {
        bail!("there is already a seat named {name}; `arakun seat rm {name}` first");
    }
    let token = read()?;
    check_token(&token)?;
    let usage = check(token.trim()).map_err(|e| {
        anyhow!(
            "the token did not work, so it was not saved: {}",
            plan::hide_token(&format!("{e:#}"), token.trim())
        )
    })?;
    store.add(name, &token)?;
    writeln!(
        out,
        "Added seat {name}: {}. Its token is in {}.",
        summary(Some(&usage)),
        store.path(name).display()
    )?;
    Ok(usage)
}

/// One line of the token from stdin; not echoed when stdin is a terminal.
fn read_token(stdin: &std::io::Stdin) -> Result<String> {
    use std::io::IsTerminal as _;
    let tty = stdin.is_terminal();
    if tty {
        eprint!("Paste the token `claude setup-token` printed (it is not shown): ");
    }
    let mut line = String::new();
    {
        let _quiet = tty.then(NoEcho::start);
        stdin.lock().read_line(&mut line)?;
    }
    if tty {
        eprintln!();
    }
    Ok(line.trim().to_string())
}

/// `arakun seat ls`: one row per seat, its 5-hour and weekly windows. No token.
fn list(seats: &Seats, own: bool, borrowed: bool, names: &[String], now: u64) -> Vec<String> {
    let live = live();
    let mut rows = vec![format!("{:<20} {}", "SEAT", "USAGE")];
    let mut row = |id: &str, usage: Option<&PlanUsage>, note: Option<&str>| {
        let mut text = summary(usage);
        if let Some(reset) = usage
            .and_then(PlanUsage::five_hour_window)
            .and_then(|w| w.reset_text(now))
        {
            text.push_str(&format!(" · 5h {reset}"));
        }
        if let Some(n) = live.get(id).filter(|n| **n > 0) {
            text.push_str(&format!(" · {n} live"));
        }
        if let Some(note) = note {
            text.push_str(&format!(" · {note}"));
        }
        rows.push(format!("{:<20} {text}", id));
    };
    if own {
        row(OWN, seats.own.as_ref(), Some("the claude.ai sign-in"));
    }
    if borrowed {
        row(
            BORROWED,
            seats.borrowed.as_ref(),
            Some("CLAUDE_CODE_OAUTH_TOKEN"),
        );
    }
    for name in names {
        let known = seats.named.iter().find(|s| s.name == *name);
        row(
            name,
            known.and_then(|s| s.usage.as_ref()),
            known.and_then(|s| s.error.as_deref()),
        );
    }
    if names.is_empty() {
        rows.push(
            "No named seats: sessions start as before. Add one with `arakun seat add <name>`."
                .into(),
        );
    }
    rows
}

/// "5h 12% · week 40%", or "not read yet".
fn summary(usage: Option<&PlanUsage>) -> String {
    let Some(u) = usage else {
        return "not read yet".into();
    };
    let pct = |w: Option<&plan::Window>| w.map_or("?".to_string(), |w| format!("{}%", w.percent));
    format!(
        "5h {} · week {}",
        pct(u.five_hour_window()),
        pct(u.weekly())
    )
}

/// Turns the terminal's echo off until dropped, so a pasted token never
/// shows on screen or in the scrollback.
struct NoEcho {
    #[cfg(unix)]
    saved: Option<libc::termios>,
    #[cfg(windows)]
    saved: Option<u32>,
}

impl NoEcho {
    #[cfg(unix)]
    fn start() -> Self {
        // SAFETY: tcgetattr fills the zeroed struct for fd 0, and tcsetattr
        // only reads it; both fail harmlessly when fd 0 is not a terminal.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return NoEcho { saved: None };
            }
            let saved = t;
            t.c_lflag &= !libc::ECHO;
            libc::tcsetattr(0, libc::TCSANOW, &t);
            NoEcho { saved: Some(saved) }
        }
    }

    #[cfg(windows)]
    fn start() -> Self {
        use windows_sys::Win32::System::Console::{
            ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
        };
        // SAFETY: the handle is the process's own stdin; GetConsoleMode fails
        // when it is not a console, and then nothing is changed.
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return NoEcho { saved: None };
            }
            SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT);
            NoEcho { saved: Some(mode) }
        }
    }

    #[cfg(not(any(unix, windows)))]
    fn start() -> Self {
        NoEcho {}
    }
}

impl Drop for NoEcho {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(saved) = &self.saved {
            // SAFETY: restores the settings read in `start`.
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, saved);
            }
        }
        #[cfg(windows)]
        if let Some(mode) = self.saved {
            use windows_sys::Win32::System::Console::{
                GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
            };
            // SAFETY: restores the mode read in `start` on the same handle.
            unsafe {
                SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), mode);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::plan::{NamedSeat, Window};
    use std::path::Path;

    const SECRET: &str = "sk-ant-oat01-NOT-A-REAL-TOKEN-abc123";

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-seats-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn usage(five_hour: u64, weekly: u64) -> PlanUsage {
        let window = |label: &str, percent: u64| Window {
            label: label.into(),
            percent,
            severity: "normal".into(),
            resets_at: None,
            active: true,
        };
        PlanUsage {
            at: 1,
            windows: vec![
                window("5-hour limit", five_hour),
                window("Weekly limit", weekly),
            ],
            borrowed: true,
            ..PlanUsage::default()
        }
    }

    fn named(name: &str, usage: Option<PlanUsage>) -> NamedSeat {
        NamedSeat {
            name: name.into(),
            at: 1,
            usage,
            error: None,
        }
    }

    fn cand(id: &str, five_hour: Option<u64>, near: bool, live: usize) -> Candidate {
        Candidate {
            id: id.into(),
            five_hour,
            near,
            live,
        }
    }

    #[test]
    fn picks_the_seat_with_the_most_room_not_near_its_limit() {
        let c = [
            cand(OWN, Some(60), false, 0),
            cand("a", Some(20), false, 0),
            cand("b", Some(5), true, 0),
        ];
        // "b" has more room, but its weekly window is near the limit.
        assert_eq!(
            pick(&c),
            Some(Picked {
                id: "a".into(),
                crowded: false
            })
        );
    }

    #[test]
    fn live_sessions_count_against_a_seat() {
        // Same window, but "a" already runs two sessions.
        let c = [cand("a", Some(10), false, 2), cand("b", Some(10), false, 0)];
        assert_eq!(pick(&c).unwrap().id, "b");
        // Ties go to the first, so the sign-in is used before the others.
        let c = [cand(OWN, None, false, 0), cand("a", Some(0), false, 0)];
        assert_eq!(pick(&c).unwrap().id, OWN);
    }

    #[test]
    fn when_every_seat_is_near_its_limit_the_least_full_is_crowded() {
        let c = [cand("a", Some(95), true, 0), cand("b", Some(85), true, 0)];
        assert_eq!(
            pick(&c),
            Some(Picked {
                id: "b".into(),
                crowded: true
            })
        );
        assert_eq!(pick(&[]), None);
    }

    #[test]
    fn candidates_skip_a_seat_whose_token_never_answered() {
        let seats = Seats {
            own: Some(usage(50, 10)),
            named: vec![
                named("dead", None),
                named("fresh", Some(usage(3, 1))),
                NamedSeat {
                    error: Some("HTTP 401".into()),
                    ..named("broken", None)
                },
                NamedSeat {
                    error: Some("timeout".into()),
                    ..named("flaky", Some(usage(90, 20)))
                },
            ],
            ..Seats::default()
        };
        let names: Vec<String> = ["broken", "dead", "flaky", "fresh", "new"]
            .map(String::from)
            .into();
        let c = candidates(&seats, true, false, &names, &HashMap::new());
        let ids: Vec<&str> = c.iter().map(|c| c.id.as_str()).collect();
        // "broken" never worked; "flaky" failed once but kept its windows;
        // "new" was never read and counts as empty.
        assert_eq!(ids, [OWN, "dead", "flaky", "fresh", "new"]);
        assert!(c[2].near);
        assert_eq!(c[4].five_hour, None);
    }

    fn tokens(id: &str) -> Option<Option<String>> {
        match id {
            OWN => Some(None),
            "a" => Some(Some(format!("{SECRET}-a"))),
            "b" => Some(Some(format!("{SECRET}-b"))),
            _ => None,
        }
    }

    #[test]
    fn a_new_session_gets_the_emptiest_seat_and_its_token() {
        let names: Vec<String> = vec!["a".into(), "b".into()];
        let seats = Seats {
            own: Some(usage(70, 10)),
            named: vec![
                named("a", Some(usage(40, 10))),
                named("b", Some(usage(5, 10))),
            ],
            ..Seats::default()
        };
        let a = decide(None, &names, true, false, &seats, &HashMap::new(), tokens).unwrap();
        assert_eq!(a.seat, "b");
        assert_eq!(a.notice, None);
        let (set, unset) = a.env();
        assert_eq!(
            set,
            [(TOKEN_VAR.to_string(), OsString::from(format!("{SECRET}-b")))]
        );
        assert!(unset.is_empty());
        // The token never shows in a log line of it.
        assert!(!format!("{a:?}").contains(SECRET));
    }

    #[test]
    fn a_resumed_session_keeps_its_seat() {
        let names: Vec<String> = vec!["a".into(), "b".into()];
        // "a" is now the fullest, and near its limit, but the session is pinned to it.
        let seats = Seats {
            own: Some(usage(0, 0)),
            named: vec![
                named("a", Some(usage(97, 10))),
                named("b", Some(usage(0, 0))),
            ],
            ..Seats::default()
        };
        let live = HashMap::from([("b".to_string(), 0)]);
        let a = decide(Some("a"), &names, true, false, &seats, &live, tokens).unwrap();
        assert_eq!((a.seat.as_str(), a.notice.as_deref()), ("a", None));
        // The sign-in runs with no token at all, even one Arakun was started with.
        let a = decide(Some(OWN), &names, true, false, &seats, &live, tokens).unwrap();
        assert_eq!(a.seat, OWN);
        assert_eq!(a.env(), (Vec::new(), vec![TOKEN_VAR.to_string()]));
    }

    #[test]
    fn a_session_whose_seat_is_gone_moves_and_says_so() {
        let names: Vec<String> = vec!["a".into()];
        let seats = Seats {
            own: Some(usage(90, 10)),
            named: vec![named("a", Some(usage(95, 10)))],
            ..Seats::default()
        };
        let a = decide(
            Some("gone"),
            &names,
            true,
            false,
            &seats,
            &HashMap::new(),
            tokens,
        )
        .unwrap();
        assert_eq!(a.seat, OWN);
        let notice = a.notice.unwrap();
        assert!(notice.contains("Seat gone is gone"), "{notice}");
        assert!(notice.contains("near its limit"), "{notice}");
    }

    #[test]
    fn without_named_seats_sessions_start_as_before() {
        let seats = Seats::default();
        assert!(decide(None, &[], true, true, &seats, &HashMap::new(), tokens).is_none());
    }

    #[test]
    fn leases_count_live_sessions_until_dropped() {
        let seat = "lease-test-seat";
        let one = lease(seat);
        let two = lease(seat);
        assert_eq!(live().get(seat), Some(&2));
        assert_eq!(one.seat(), seat);
        drop(one);
        assert_eq!(live().get(seat), Some(&1));
        drop(two);
        assert_eq!(live().get(seat), None);
    }

    #[test]
    fn names_are_file_names_and_not_the_other_seats() {
        for ok in ["work", "max-2", "a_b", "x"] {
            assert!(valid_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "own", "borrowed", "Work", "../x", "a/b", "a b", ".hidden", "-x", "é",
        ] {
            assert!(valid_name(bad).is_err(), "{bad}");
        }
        assert!(valid_name(&"a".repeat(33)).is_err());
    }

    #[test]
    fn the_store_round_trips_and_keeps_tokens_private() {
        let dir = scratch("store");
        let store = Store::new(dir.clone());
        assert!(store.names().is_empty());
        store.add("work", &format!("  {SECRET}\n")).unwrap();
        store.add("spare", "tok-2").unwrap();
        // A temp file a crash left behind is not a seat.
        std::fs::write(dir.join(".work.123-0.tmp"), "x").unwrap();
        assert_eq!(store.names(), ["spare", "work"]);
        assert_eq!(store.token("work").as_deref(), Some(SECRET));
        assert_eq!(store.token("../work"), None);
        assert!(store.add("own", "x").is_err());
        assert!(store.add("work", "two words").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir.join("work")), 0o600);
            assert_eq!(mode(&dir), 0o700);
        }
        assert!(store.remove("work").unwrap());
        assert!(!store.remove("work").unwrap());
        assert_eq!(store.names(), ["spare"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adding_a_seat_never_prints_its_token() {
        let dir = scratch("add");
        let store = Store::new(dir.clone());
        let mut out = Vec::new();
        // A check that fails, quoting the token: neither the error nor the
        // output carries it, and nothing is saved.
        let err = add(
            &store,
            "work",
            || Ok(SECRET.into()),
            |t| Err(anyhow!("401 for {t}")),
            &mut out,
        )
        .unwrap_err();
        assert!(!format!("{err:#}").contains(SECRET), "{err:#}");
        assert!(store.names().is_empty());
        let got = add(
            &store,
            "work",
            || Ok(format!("{SECRET}\n")),
            |_| Ok(usage(12, 40)),
            &mut out,
        )
        .unwrap();
        assert_eq!(got.five_hour(), Some(12));
        assert_eq!(store.token("work").as_deref(), Some(SECRET));
        let printed = String::from_utf8(out).unwrap();
        assert!(
            printed.contains("Added seat work: 5h 12% · week 40%"),
            "{printed}"
        );
        assert!(!printed.contains(SECRET), "{printed}");
        // A second add of the same name doesn't replace it.
        assert!(
            add(
                &store,
                "work",
                || Ok("other".into()),
                |_| Ok(usage(0, 0)),
                &mut Vec::new()
            )
            .is_err()
        );
        // Listing shows names and windows, never a token.
        let seats = Seats {
            named: vec![named("work", Some(got))],
            ..Seats::default()
        };
        let rows = list(&seats, true, false, &store.names(), 0).join("\n");
        assert!(rows.contains("work") && rows.contains("5h 12%"), "{rows}");
        assert!(!rows.contains(SECRET), "{rows}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_settings_notice_says_how_to_fix_it() {
        let n = Blocked::SettingsToken.notice();
        assert!(n.contains("settings.json") && n.contains(TOKEN_VAR));
        assert!(n.contains("arakun seat add"));
    }
}
