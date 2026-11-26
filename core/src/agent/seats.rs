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
//!
//! A session is pinned to its seat by name and by a fingerprint of the
//! seat's token, so a token swapped under the same name is noticed. Before a
//! session starts on a named seat, its token is checked with one small read.
//! A session never moves to another account without a notice that says so,
//! and sessions in the sandbox leave seats out.
//!
//! A seat's token is kept in the keyring (`secret_store`); its file in
//! `seats/` only says the seat is there.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use sha2::{Digest as _, Sha256};

use super::claude_auth;
use super::plan::{self, PlanUsage, Seats};
use crate::secret::Secret;

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
/// How long a session start waits for the seats' windows to be read before
/// it picks on the last ones known. Reads that take longer still land in
/// the cache for the next pick.
const FRESH_WAIT: Duration = Duration::from_secs(4);
/// A good read of a named seat this recent counts as the check before a
/// session starts on it, so a seat a pick has just read is not read twice.
const CHECKED_FOR: u64 = 60;
/// How long a session start waits for that check before it starts anyway
/// and says the token could not be checked.
const CHECK_WAIT: Duration = Duration::from_secs(10);

/// A one-way fingerprint of a seat's token: SHA-256, in hex. Saved with the
/// session instead of the token, to notice a token swapped under one name.
pub fn fingerprint(token: &str) -> String {
    Sha256::digest(token.trim().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The seat's name as people read it.
pub fn label(id: &str) -> String {
    match id {
        OWN => "Sign-in".into(),
        BORROWED => "Borrowed token".into(),
        name => name.into(),
    }
}

/// The seat as a notice names it, mid-sentence.
fn called(id: &str) -> String {
    match id {
        OWN => "the sign-in".into(),
        BORROWED => "the borrowed token".into(),
        name => format!("seat {name}"),
    }
}

// ---- storage -------------------------------------------------------------

/// Where named seats are listed: one file per seat, named after it.
pub fn dir() -> PathBuf {
    crate::platform::app_dir().join("seats")
}

/// The keyring entry of seat `name`'s token. Not per test Arakun: the seats'
/// files are shared too.
fn account(name: &str) -> String {
    format!("seat:{name}")
}

/// The named seats. Each has an empty file in `dir` (0600 in a 0700 folder on
/// Unix), which lists it, and its token in the keyring. A file from before
/// holds the token itself, until it is moved to the keyring.
pub struct Store {
    dir: PathBuf,
    keep: Keep,
    /// Seats the keyring failed for while this store was open. It is not
    /// asked for them again: one session start reads a token several times,
    /// and a locked keyring would show its unlock prompt each time.
    failed: Mutex<HashSet<String>>,
}

/// Where the tokens are kept: the keyring, or one in memory in the tests.
enum Keep {
    Keyring,
    #[cfg(test)]
    Fake(std::sync::Arc<Mutex<tests::FakeRing>>),
}

impl Keep {
    fn get(&self, account: &str) -> std::result::Result<Option<String>, String> {
        match self {
            Keep::Keyring => crate::secret_store::get(account),
            #[cfg(test)]
            Keep::Fake(ring) => lock(ring).get(account),
        }
    }

    fn set(&self, account: &str, secret: &str) -> std::result::Result<(), String> {
        match self {
            Keep::Keyring => crate::secret_store::set(account, "arakun: Claude seat token", secret),
            #[cfg(test)]
            Keep::Fake(ring) => lock(ring).set(account, secret),
        }
    }

    fn forget(&self, account: &str) {
        match self {
            Keep::Keyring => crate::secret_store::forget(account),
            #[cfg(test)]
            Keep::Fake(ring) => {
                lock(ring).entries.remove(account);
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Self::with(dir, Keep::Keyring)
    }

    fn with(dir: PathBuf, keep: Keep) -> Self {
        Store {
            dir,
            keep,
            failed: Mutex::new(HashSet::new()),
        }
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

    /// Seat `name`'s token, `None` when there is none or the keyring could
    /// not give it. Not on the UI thread: the keyring may wait for the person
    /// to unlock it.
    pub fn token(&self, name: &str) -> Option<String> {
        self.read(name).unwrap_or_else(|e| {
            eprintln!("arakun: the keyring could not give seat {name}'s token: {e}");
            None
        })
    }

    /// [`token`](Self::token), with an error when the keyring could not say.
    /// A token still in the seat's file is moved to the keyring first.
    pub(super) fn read(&self, name: &str) -> std::result::Result<Option<String>, String> {
        if valid_name(name).is_err() {
            return Ok(None);
        }
        let path = self.path(name);
        // No file, no seat: the keyring is not asked.
        let Ok(in_file) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        let in_file = in_file.trim();
        if !in_file.is_empty() {
            if !lock(&self.failed).contains(name) {
                self.move_to_keyring(name, in_file, &path);
            }
            return Ok(Some(in_file.to_string()));
        }
        if lock(&self.failed).contains(name) {
            return Err("it did not answer before".into());
        }
        match self.keep.get(&account(name)) {
            Ok(t) => Ok(t.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())),
            Err(e) => {
                lock(&self.failed).insert(name.to_string());
                Err(e)
            }
        }
    }

    /// Moves seat `name`'s token from its file to the keyring, and empties
    /// the file. When the keyring can't take it, it stays in the file and is
    /// still used.
    fn move_to_keyring(&self, name: &str, token: &str, path: &Path) {
        if let Err(e) = self.keep.set(&account(name), token) {
            eprintln!("arakun: could not move seat {name}'s token to the keyring: {e}");
            lock(&self.failed).insert(name.to_string());
            return;
        }
        // Emptied in place, not written anew: a seat removed meanwhile must
        // not come back.
        if let Err(e) = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("arakun: could not empty {}: {e}", path.display());
        }
    }

    /// Saves `token` as seat `name`, replacing one of that name: the token
    /// in the keyring, an empty file to list the seat.
    pub fn add(&self, name: &str, token: &str) -> Result<()> {
        valid_name(name)?;
        check_token(token)?;
        self.keep
            .set(&account(name), token.trim())
            .map_err(|e| anyhow!("the keyring could not keep the token: {e}"))?;
        lock(&self.failed).remove(name);
        crate::durable::write_private(&self.path(name), b"")?;
        Ok(())
    }

    /// Whether there was a seat to remove.
    pub fn remove(&self, name: &str) -> Result<bool> {
        valid_name(name)?;
        let was = match std::fs::remove_file(self.path(name)) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        self.keep.forget(&account(name));
        Ok(was)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

/// Names Windows keeps for devices: a file can't have one, with any suffix.
const DEVICES: [&str; 4] = ["con", "prn", "aux", "nul"];

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
    // `com1` to `com9` and `lpt1` to `lpt9` too. Refused everywhere, so a
    // seat added on one system can be copied to another.
    let device = DEVICES.contains(&name)
        || ((name.starts_with("com") || name.starts_with("lpt"))
            && name.len() == 4
            && name.as_bytes()[3].is_ascii_digit()
            && name.as_bytes()[3] != b'0');
    if device {
        bail!("\"{name}\" names a device on Windows; pick another name");
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
#[derive(Debug)]
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
        live_lock().retain(|(id, _)| *id != self.id);
    }
}

fn live_lock() -> MutexGuard<'static, Vec<(u64, String)>> {
    LIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Counts a session as live on `seat` until the lease is dropped.
pub fn lease(seat: &str) -> Lease {
    lease_in(&mut live_lock(), seat)
}

fn lease_in(live: &mut Vec<(u64, String)>, seat: &str) -> Lease {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    live.push((id, seat.to_string()));
    Lease {
        id,
        seat: seat.to_string(),
    }
}

/// How many live sessions each seat has.
pub fn live() -> HashMap<String, usize> {
    counts(&live_lock())
}

fn counts(live: &[(u64, String)]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for (_, seat) in live {
        *counts.entry(seat.clone()).or_insert(0) += 1;
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
    /// From the seat's last known windows, as they stand at `now`: one that
    /// has started over since it was read is empty.
    fn new(id: &str, usage: Option<&PlanUsage>, live: &HashMap<String, usize>, now: u64) -> Self {
        Candidate {
            id: id.to_string(),
            five_hour: usage.and_then(|u| u.five_hour_at(now)),
            near: usage.is_some_and(|u| u.near_limit(now)),
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
/// token, then the named seats. A named seat whose token never answered, or
/// was refused at its last read, is left out; one not read yet is in, as
/// empty.
fn candidates(
    seats: &Seats,
    own: bool,
    borrowed: bool,
    names: &[String],
    live: &HashMap<String, usize>,
    now: u64,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    if own {
        out.push(Candidate::new(OWN, seats.own.as_ref(), live, now));
    }
    if borrowed {
        out.push(Candidate::new(BORROWED, seats.borrowed.as_ref(), live, now));
    }
    for name in names {
        let known = seats.named.iter().find(|s| s.name == *name);
        if known.is_some_and(|s| s.refused() || (s.usage.is_none() && s.error.is_some())) {
            continue;
        }
        out.push(Candidate::new(
            name,
            known.and_then(|s| s.usage.as_ref()),
            live,
            now,
        ));
    }
    out
}

/// Whether the last read of named seat `id` was refused.
fn refused(seats: &Seats, id: &str) -> bool {
    seats.named.iter().any(|s| s.name == id && s.refused())
}

/// The seat a session runs on, and what that means for its environment.
#[derive(Debug)]
pub struct Assignment {
    pub seat: String,
    /// The seat's token; `None` for the sign-in, which runs with none.
    token: Option<Secret>,
    /// The token's [`fingerprint`], saved with the session; `None` for the
    /// sign-in, whose token Claude Code renews on its own.
    // Not a secret: a fingerprint of the token.
    pub key: Option<String>,
    /// Something to tell the session: its seat is gone, or every seat is full.
    pub notice: Option<String>,
    /// The session counts as live on the seat from the moment it is picked,
    /// so a pick made at the same time sees it. Dropped with the assignment
    /// if the session never starts.
    pub lease: Option<Lease>,
}

impl Assignment {
    /// A named seat's token, which is checked before the session starts.
    fn named_token(&self) -> Option<&str> {
        (self.seat != BORROWED).then_some(self.token.as_ref().map(Secret::expose))?
    }

    fn add_notice(&mut self, text: String) {
        self.notice = Some(match self.notice.take() {
            Some(n) => format!("{n} {text}"),
            None => text,
        });
    }

    /// What the session's process gets: the seat's token, or none at all for
    /// the sign-in (one in Arakun's own environment would win over it).
    pub fn env(&self) -> (Vec<(String, OsString)>, Vec<String>) {
        match &self.token {
            Some(t) => (vec![(TOKEN_VAR.into(), t.expose().into())], Vec::new()),
            None => (Vec::new(), vec![TOKEN_VAR.into()]),
        }
    }
}

/// Where a Claude Code session starts.
#[derive(Debug)]
pub enum Seating {
    /// On a seat.
    On(Assignment),
    /// As it would without seats. The notice says why, when the session was
    /// on a seat and now runs on another account.
    Off(Option<String>),
}

/// The seat a Claude Code session starts on: `pinned`, the one saved with
/// it, while that seat is still there and its token was not refused; else
/// the one [`pick`] finds. `key` is the pinned token's [`fingerprint`]. A
/// named seat's token is checked first ([`settle`]).
///
/// Sessions in the sandbox, and all of them while seats can't work
/// ([`blocked`]), start as they would without seats, and one that was on a
/// seat says so. The resolver that calls this runs off the UI thread.
pub fn assign(pinned: Option<&str>, key: Option<&str>, sandboxed: bool) -> Seating {
    // The sign-in is what a session runs on without seats, unless Arakun
    // has a token of its own or Claude Code runs on an API key, a gateway
    // or a cloud provider.
    let moved = |p: &str| p != OWN || plan::active_is_borrowed() || claude_auth::access().is_some();
    if sandboxed {
        return Seating::Off(pinned.filter(|p| moved(p)).map(|p| {
            format!(
                "This session runs in the sandbox, which does not use seats, so it runs on \
                 Claude Code's default account instead of {}.",
                called(p)
            )
        }));
    }
    if let Some(b) = blocked() {
        return Seating::Off(pinned.filter(|p| moved(p)).map(|p| {
            format!(
                "{} This session runs on Claude Code's default account instead of {}.",
                b.notice(),
                called(p)
            )
        }));
    }
    let store = Store::open();
    settle(
        pinned,
        store.names().len() + 2,
        || choose(pinned, key, &store),
        check,
        |seat, token| store.token(seat).as_deref() == Some(token),
    )
}

/// The seat [`choose`] finds, once its token has passed `check`. One that is
/// refused is marked so by the check, and the session chooses again. One that
/// is saved anew while it is checked (`still` is false) too. A check that
/// can't finish starts the session anyway, with a notice. At most `tries`
/// choices.
fn settle(
    pinned: Option<&str>,
    tries: usize,
    mut choose: impl FnMut() -> Option<Assignment>,
    mut check: impl FnMut(&str, &str) -> std::result::Result<(), String>,
    still: impl Fn(&str, &str) -> bool,
) -> Seating {
    let mut skipped: Vec<String> = Vec::new();
    for _ in 0..tries {
        let Some(mut a) = choose() else {
            break;
        };
        let Some(token) = a.named_token().map(str::to_string) else {
            return Seating::On(with_skipped(a, &skipped));
        };
        match check(&a.seat, &token) {
            Ok(()) if still(&a.seat, &token) => return Seating::On(with_skipped(a, &skipped)),
            Ok(()) => {}
            // A pinned session's move says so itself.
            Err(e) if plan::is_refusal(&e) => {
                if pinned != Some(a.seat.as_str()) && !skipped.contains(&a.seat) {
                    skipped.push(a.seat.clone());
                }
            }
            Err(e) => {
                a.add_notice(format!(
                    "{}'s token could not be checked before this session started ({}), so a \
                     problem with it shows only once the session runs.",
                    upper(&called(&a.seat)),
                    plan::hide_token(&e, &token)
                ));
                return Seating::On(with_skipped(a, &skipped));
            }
        }
    }
    let why = pinned.map(|p| {
        format!(
            "No Claude seat can take this session: {} can't be used and no other seat \
             works, so it starts as it would without seats.",
            called(p)
        )
    });
    Seating::Off(why.or_else(|| skipped_notice(&skipped)))
}

/// `a`, saying which seats were skipped on the way to it.
fn with_skipped(mut a: Assignment, skipped: &[String]) -> Assignment {
    if let Some(text) = skipped_notice(skipped) {
        let rest = a.notice.take();
        a.notice = Some(text);
        if let Some(rest) = rest {
            a.add_notice(rest);
        }
    }
    a
}

fn skipped_notice(skipped: &[String]) -> Option<String> {
    let (first, rest) = skipped.split_first()?;
    let names = rest
        .iter()
        .fold(first.clone(), |all, n| format!("{all}, {n}"));
    Some(format!(
        "Skipped {} {names}: the token was refused. Run `arakun seat rm <name>` and add it again \
         with a new token from `claude setup-token`.",
        if rest.is_empty() { "seat" } else { "seats" },
    ))
}

fn upper(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

/// One read of named seat `seat` with `token`, kept in the cache, waiting at
/// most [`CHECK_WAIT`]. The error never carries the token.
fn check(seat: &str, token: &str) -> std::result::Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (seat, token) = (seat.to_string(), token.to_string());
    std::thread::Builder::new()
        .name("seat-check".into())
        .spawn(move || {
            let got = plan::check_named(&seat, &token, CHECKED_FOR);
            let _ = tx.send(got.map(|_| ()).map_err(|e| format!("{e:#}")));
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(CHECK_WAIT)
        .unwrap_or_else(|_| Err(format!("no answer within {} seconds", CHECK_WAIT.as_secs())))
}

/// [`assign`]'s choice before the check. Only a pick reads windows, and only
/// those of the seats it chooses between that are stale past
/// [`plan::seats_probing`]'s bounds, waiting at most [`FRESH_WAIT`]. The
/// session is counted on its seat in the same step as the pick.
fn choose(pinned: Option<&str>, key: Option<&str>, store: &Store) -> Option<Assignment> {
    let names = store.names();
    let own = plan::own_sign_in();
    let borrowed = plan::borrowed_seat_token();
    let token = |id: &str| match id {
        OWN => own.then_some(None),
        BORROWED => borrowed.clone().map(Some),
        name => store.token(name).map(Some),
    };
    let known = plan::known_seats();
    let now = super::rpc::unix_now();
    let keeps = keeps_pin(pinned, &known, token) || (pinned.is_none() && names.is_empty());
    let seats = if keeps {
        known
    } else {
        let choices: Vec<String> = candidates(
            &known,
            own,
            borrowed.is_some(),
            &names,
            &HashMap::new(),
            now,
        )
        .into_iter()
        .map(|c| c.id)
        .filter(|id| names.contains(id))
        .collect();
        fresh(choices, FRESH_WAIT).unwrap_or(known)
    };
    let mut live = live_lock();
    let mut a = decide(
        pinned,
        key,
        &names,
        own,
        borrowed.is_some(),
        &seats,
        &counts(&live),
        now,
        token,
    )?;
    a.lease = Some(lease_in(&mut live, &a.seat));
    Some(a)
}

/// The seats' windows with the stale ones in `probe` read again, or `None`
/// when that takes longer than `wait`.
fn fresh(probe: Vec<String>, wait: Duration) -> Option<Seats> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("seat-windows".into())
        .spawn(move || {
            let _ = tx.send(plan::seats_probing(&probe));
        })
        .ok()?;
    rx.recv_timeout(wait).ok()
}

/// Whether a session saved on `pinned` stays there: the seat is still saved
/// and its token was not refused at its last read.
fn keeps_pin(
    pinned: Option<&str>,
    seats: &Seats,
    token: impl Fn(&str) -> Option<Option<String>>,
) -> bool {
    pinned.is_some_and(|p| token(p).is_some() && !refused(seats, p))
}

/// [`assign`] without the files. `token` says whether a seat is there and
/// its token: `Some(None)` for the sign-in. `key` is the pinned token's
/// [`fingerprint`] as saved with the session.
#[allow(clippy::too_many_arguments)]
fn decide(
    pinned: Option<&str>,
    key: Option<&str>,
    names: &[String],
    own: bool,
    borrowed: bool,
    seats: &Seats,
    live: &HashMap<String, usize>,
    now: u64,
    token: impl Fn(&str) -> Option<Option<String>>,
) -> Option<Assignment> {
    if let Some(seat) = pinned
        && keeps_pin(pinned, seats, &token)
        && let Some(t) = token(seat)
    {
        let now_key = t.as_deref().map(fingerprint);
        // Sessions saved before fingerprints have none, and take this one.
        let swapped = key.is_some() && now_key.is_some() && key != now_key.as_deref();
        let notice = swapped.then(|| {
            format!(
                "The token of {} changed since this session last ran, so the session may be \
                 on another Claude account now. It goes on with the new token.",
                called(seat)
            )
        });
        return Some(Assignment {
            seat: seat.to_string(),
            key: now_key,
            token: t.map(Secret::from),
            notice,
            lease: None,
        });
    }
    // Opt-in: without a named seat, a new session starts as it always did.
    if pinned.is_none() && names.is_empty() {
        return None;
    }
    let picked = pick(&candidates(seats, own, borrowed, names, live, now))?;
    let t = token(&picked.id)?;
    let on = label(&picked.id);
    let mut notice = pinned.map(|p| {
        if refused(seats, p) {
            format!(
                "Seat {p}'s token was refused, so this session now runs on {on}. \
                 Run `arakun seat rm {p}` and add it again with a new token from \
                 `claude setup-token`."
            )
        } else {
            format!(
                "Seat {} is gone, so this session now runs on {on}.",
                label(p)
            )
        }
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
        key: t.as_deref().map(fingerprint),
        token: t.map(Secret::from),
        notice,
        lease: None,
    })
}

// ---- command line --------------------------------------------------------

const USAGE: &str = "usage: arakun seat add <name>   (the token from `claude setup-token` on stdin)
       arakun seat ls
       arakun seat rm <name>";

/// `arakun seat …`: the exit code.
pub fn cli(args: &[String]) -> i32 {
    // `seat` runs before the generic help check, and a seat name never
    // starts with a dash, so `--help` anywhere asks for the usage.
    if args.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
        println!("{USAGE}");
        return 0;
    }
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
            .map(|usage| {
                plan::remember_named(name, usage, store.token(name).map(|t| fingerprint(&t)))
            })
        }
        [cmd] if cmd == "ls" => {
            let seats = plan::seats_probing(&store.names());
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
    // A keyring that could not say is not read as no seat: adding would
    // replace one.
    match store.read(name) {
        Ok(None) => {}
        Ok(Some(_)) => {
            bail!("there is already a seat named {name}; `arakun seat rm {name}` first")
        }
        Err(e) => bail!("the keyring could not say whether seat {name} is there: {e}"),
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
    writeln!(out, "Added seat {name}: {}.", summary(Some(&usage)))?;
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

    const SECRET: &str = "sk-ant-oat01-NOT-A-REAL-TOKEN-abc123";

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-seats-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A keyring in memory. `locked` makes it refuse, as one left locked does.
    #[derive(Default)]
    pub(super) struct FakeRing {
        pub(super) entries: HashMap<String, String>,
        locked: bool,
        asked: usize,
    }

    impl FakeRing {
        pub(super) fn get(&mut self, account: &str) -> std::result::Result<Option<String>, String> {
            self.asked += 1;
            if self.locked {
                return Err("locked".into());
            }
            Ok(self.entries.get(account).cloned())
        }

        pub(super) fn set(
            &mut self,
            account: &str,
            secret: &str,
        ) -> std::result::Result<(), String> {
            self.asked += 1;
            if self.locked {
                return Err("locked".into());
            }
            self.entries.insert(account.into(), secret.into());
            Ok(())
        }
    }

    type Ring = std::sync::Arc<Mutex<FakeRing>>;

    /// A store in `dir` on a keyring in memory, never the person's own.
    fn store_in(dir: &Path) -> (Store, Ring) {
        let ring = Ring::default();
        (
            Store::with(dir.to_path_buf(), Keep::Fake(ring.clone())),
            ring,
        )
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
            key: None,
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
        let c = candidates(&seats, true, false, &names, &HashMap::new(), 0);
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
        let a = decide(
            None,
            None,
            &names,
            true,
            false,
            &seats,
            &HashMap::new(),
            0,
            tokens,
        )
        .unwrap();
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
        let a = decide(
            Some("a"),
            None,
            &names,
            true,
            false,
            &seats,
            &live,
            0,
            tokens,
        )
        .unwrap();
        assert_eq!((a.seat.as_str(), a.notice.as_deref()), ("a", None));
        // The sign-in runs with no token at all, even one Arakun was started with.
        let a = decide(
            Some(OWN),
            None,
            &names,
            true,
            false,
            &seats,
            &live,
            0,
            tokens,
        )
        .unwrap();
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
            None,
            &names,
            true,
            false,
            &seats,
            &HashMap::new(),
            0,
            tokens,
        )
        .unwrap();
        assert_eq!(a.seat, OWN);
        let notice = a.notice.unwrap();
        assert!(notice.contains("Seat gone is gone"), "{notice}");
        assert!(notice.contains("near its limit"), "{notice}");
    }

    #[test]
    fn a_window_that_started_over_since_it_was_read_is_empty() {
        let mut u = usage(95, 10);
        u.windows[0].resets_at = Some(1_000);
        let live = HashMap::new();
        let before = Candidate::new("a", Some(&u), &live, 999);
        assert_eq!((before.five_hour, before.near), (Some(95), true));
        // Same cached usage, read after the reset: not near, and its 95% no
        // longer counts against its room.
        let after = Candidate::new("a", Some(&u), &live, 1_000);
        assert_eq!((after.five_hour, after.near), (None, false));
        assert_eq!(after.room(), 100);
        // A weekly window near its limit still counts until it resets too.
        let mut u = usage(10, 90);
        u.windows[1].resets_at = Some(5_000);
        assert!(Candidate::new("a", Some(&u), &live, 4_999).near);
        assert!(!Candidate::new("a", Some(&u), &live, 5_000).near);
    }

    #[test]
    fn a_session_on_a_refused_seat_moves_and_names_it() {
        let names: Vec<String> = vec!["a".into(), "b".into()];
        let seats = Seats {
            own: Some(usage(50, 10)),
            named: vec![
                NamedSeat {
                    error: Some(
                        "HTTP 401: the token's answer carried no rate-limit headers".into(),
                    ),
                    ..named("a", Some(usage(0, 0)))
                },
                named("b", Some(usage(20, 10))),
            ],
            ..Seats::default()
        };
        // "a" looks emptiest from its last good read, but its token is dead.
        let a = decide(
            Some("a"),
            None,
            &names,
            true,
            false,
            &seats,
            &HashMap::new(),
            0,
            tokens,
        )
        .unwrap();
        assert_eq!(a.seat, "b");
        let notice = a.notice.unwrap();
        assert!(notice.contains("Seat a's token was refused"), "{notice}");
        assert!(notice.contains("arakun seat rm a"), "{notice}");
        // New sessions don't pick it either.
        let a = decide(
            None,
            None,
            &names,
            true,
            false,
            &seats,
            &HashMap::new(),
            0,
            tokens,
        )
        .unwrap();
        assert_eq!((a.seat.as_str(), a.notice), ("b", None));
        // A seat that is only busy keeps its sessions.
        let busy = Seats {
            named: vec![NamedSeat {
                error: Some("HTTP 429".into()),
                ..named("a", Some(usage(0, 0)))
            }],
            ..seats
        };
        let a = decide(
            Some("a"),
            None,
            &names,
            true,
            false,
            &busy,
            &HashMap::new(),
            0,
            tokens,
        )
        .unwrap();
        assert_eq!((a.seat.as_str(), a.notice), ("a", None));
    }

    #[test]
    fn a_fingerprint_is_one_way_and_stable() {
        let key = fingerprint(SECRET);
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!key.contains(SECRET));
        assert_eq!(fingerprint(&format!(" {SECRET}\n")), key);
        assert_ne!(fingerprint(&format!("{SECRET}-b")), key);
    }

    #[test]
    fn a_token_swapped_under_the_seats_name_is_noticed() {
        let names: Vec<String> = vec!["a".into(), "b".into()];
        let seats = Seats {
            named: vec![named("a", Some(usage(10, 10)))],
            ..Seats::default()
        };
        let live = HashMap::new();
        let key = fingerprint(&format!("{SECRET}-a"));
        let run = |key: Option<&str>| {
            decide(
                Some("a"),
                key,
                &names,
                true,
                false,
                &seats,
                &live,
                0,
                tokens,
            )
            .unwrap()
        };
        // Same token: no notice, same key.
        let a = run(Some(&key));
        assert_eq!(
            (a.notice.as_deref(), a.key.as_deref()),
            (None, Some(key.as_str()))
        );
        // A session saved before fingerprints takes the token's quietly.
        let a = run(None);
        assert_eq!(
            (a.notice.as_deref(), a.key.as_deref()),
            (None, Some(key.as_str()))
        );
        // Another token under the name: it stays on the seat, and says so.
        let other = fingerprint("an-older-token");
        let a = run(Some(&other));
        assert_eq!(a.seat, "a");
        assert_eq!(a.key.as_deref(), Some(key.as_str()));
        let notice = a.notice.unwrap();
        assert!(notice.contains("token of seat a changed"), "{notice}");
        assert!(!notice.contains(SECRET), "{notice}");
        // The sign-in has no token to fingerprint.
        let a = decide(
            Some(OWN),
            Some(&other),
            &names,
            true,
            false,
            &seats,
            &live,
            0,
            tokens,
        );
        assert_eq!(a.map(|a| (a.key, a.notice)), Some((None, None)));
    }

    fn on(seat: &str, token: Option<&str>) -> Assignment {
        Assignment {
            seat: seat.into(),
            token: token.map(Secret::from),
            key: token.map(fingerprint),
            notice: None,
            lease: None,
        }
    }

    fn seated(s: Seating) -> Assignment {
        match s {
            Seating::On(a) => a,
            Seating::Off(n) => panic!("off: {n:?}"),
        }
    }

    #[test]
    fn a_named_seat_is_checked_before_a_session_starts_on_it() {
        let mut checked = Vec::new();
        let a = seated(settle(
            None,
            3,
            || Some(on("a", Some(SECRET))),
            |seat, token| {
                checked.push((seat.to_string(), token == SECRET));
                Ok(())
            },
            |_, _| true,
        ));
        assert_eq!((a.seat.as_str(), a.notice), ("a", None));
        assert_eq!(checked, [("a".to_string(), true)]);
        // The sign-in and the borrowed token are not: Claude Code renews the
        // one, and the other is what it runs on without seats.
        for seat in [OWN, BORROWED] {
            let token = (seat == BORROWED).then_some(SECRET);
            let a = seated(settle(
                None,
                3,
                || Some(on(seat, token)),
                |_, _| panic!("{seat} was checked"),
                |_, _| true,
            ));
            assert_eq!(a.seat, seat);
        }
    }

    #[test]
    fn a_refused_seat_is_chosen_again_and_named() {
        // A new session: "a" is refused at the check, the next choice is "b".
        let mut order = vec![on("b", Some("tok-b")), on("a", Some(SECRET))];
        let refuse_a = |seat: &str, _: &str| {
            if seat == "a" {
                Err("HTTP 401: the token's answer carried no rate-limit headers".to_string())
            } else {
                Ok(())
            }
        };
        let a = seated(settle(None, 3, || order.pop(), refuse_a, |_, _| true));
        assert_eq!(a.seat, "b");
        let notice = a.notice.unwrap();
        assert!(notice.contains("Skipped seat a"), "{notice}");
        assert!(notice.contains("arakun seat rm"), "{notice}");
        // A pinned session's move names its seat itself, so no second notice.
        let mut order = vec![on("b", Some("tok-b")), on("a", Some(SECRET))];
        let a = seated(settle(Some("a"), 3, || order.pop(), refuse_a, |_, _| true));
        assert_eq!((a.seat.as_str(), a.notice), ("b", None));
        // Nothing left to choose: a pinned session says so, never quietly.
        let mut order = vec![on("a", Some(SECRET))];
        match settle(Some("a"), 3, || order.pop(), refuse_a, |_, _| true) {
            Seating::Off(Some(n)) => assert!(n.contains("seat a can't be used"), "{n}"),
            other => panic!("{other:?}"),
        }
        let mut order = vec![on("a", Some(SECRET))];
        match settle(None, 3, || order.pop(), refuse_a, |_, _| true) {
            Seating::Off(Some(n)) => assert!(n.contains("Skipped seat a"), "{n}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_check_that_cant_finish_starts_on_the_seat_and_says_so() {
        let a = seated(settle(
            Some("a"),
            3,
            || Some(on("a", Some(SECRET))),
            |_, t| Err(format!("curl: could not resolve host, token {t}")),
            |_, _| true,
        ));
        assert_eq!(a.seat, "a");
        let notice = a.notice.unwrap();
        assert!(
            notice.starts_with("Seat a's token could not be checked"),
            "{notice}"
        );
        assert!(!notice.contains(SECRET), "{notice}");
    }

    #[test]
    fn a_seat_saved_anew_while_checked_is_chosen_again() {
        let mut chosen = 0;
        let a = seated(settle(
            None,
            3,
            || {
                chosen += 1;
                Some(on("a", Some(if chosen == 1 { "old" } else { SECRET })))
            },
            |_, _| Ok(()),
            |_, token| token == SECRET,
        ));
        assert_eq!((chosen, a.key), (2, Some(fingerprint(SECRET))));
    }

    #[test]
    fn sandboxed_sessions_leave_seats_out() {
        // Reads no file for a named seat: the pin alone says it moves.
        match assign(Some("work"), None, true) {
            Seating::Off(Some(n)) => {
                assert!(n.contains("sandbox") && n.contains("seat work"), "{n}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn without_named_seats_sessions_start_as_before() {
        let seats = Seats::default();
        assert!(
            decide(
                None,
                None,
                &[],
                true,
                true,
                &seats,
                &HashMap::new(),
                0,
                tokens
            )
            .is_none()
        );
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
        for ok in [
            "work", "max-2", "a_b", "x", "com", "com0", "com10", "console",
        ] {
            assert!(valid_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "own", "borrowed", "Work", "../x", "a/b", "a b", ".hidden", "-x", "é", "con",
            "nul", "aux", "prn", "com1", "lpt9",
        ] {
            assert!(valid_name(bad).is_err(), "{bad}");
        }
        assert!(valid_name(&"a".repeat(33)).is_err());
    }

    #[test]
    fn the_store_round_trips_and_keeps_tokens_private() {
        let dir = scratch("store");
        let (store, ring) = store_in(&dir);
        assert!(store.names().is_empty());
        store.add("work", &format!("  {SECRET}\n")).unwrap();
        store.add("spare", "tok-2").unwrap();
        // The file only lists the seat; the keyring has the token.
        assert_eq!(std::fs::read_to_string(dir.join("work")).unwrap(), "");
        assert_eq!(lock(&ring).entries["seat:work"], SECRET);
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
            use std::path::Path;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir.join("work")), 0o600);
            assert_eq!(mode(&dir), 0o700);
        }
        assert!(store.remove("work").unwrap());
        assert!(!store.remove("work").unwrap());
        assert_eq!(store.names(), ["spare"]);
        assert!(!lock(&ring).entries.contains_key("seat:work"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_token_from_before_moves_to_the_keyring() {
        let dir = scratch("move");
        let (store, ring) = store_in(&dir);
        crate::durable::write_private(&dir.join("work"), format!("{SECRET}\n").as_bytes()).unwrap();
        assert_eq!(store.token("work").as_deref(), Some(SECRET));
        assert_eq!(lock(&ring).entries["seat:work"], SECRET);
        assert_eq!(std::fs::read_to_string(dir.join("work")).unwrap(), "");
        assert_eq!(store.names(), ["work"]);
        assert_eq!(store.token("work").as_deref(), Some(SECRET));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join("work"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A seat without a file is not asked of the keyring.
        let asked = lock(&ring).asked;
        assert_eq!(store.token("gone"), None);
        assert_eq!(lock(&ring).asked, asked);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_locked_keyring_keeps_the_files_token_and_is_asked_once() {
        let dir = scratch("locked");
        let (store, ring) = store_in(&dir);
        lock(&ring).locked = true;
        crate::durable::write_private(&dir.join("old"), SECRET.as_bytes()).unwrap();
        crate::durable::write_private(&dir.join("new"), b"").unwrap();
        for _ in 0..3 {
            assert_eq!(store.token("old").as_deref(), Some(SECRET));
            assert_eq!(store.token("new"), None);
        }
        assert_eq!(
            lock(&ring).asked,
            2,
            "one prompt per seat, not one per read"
        );
        assert_eq!(std::fs::read_to_string(dir.join("old")).unwrap(), SECRET);
        // Adding over a seat the keyring can't read is refused, not a replace.
        let err = add(
            &store,
            "new",
            || Ok("tok".into()),
            |_| Ok(usage(0, 0)),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("keyring"), "{err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_seat_removed_while_its_token_moves_stays_removed() {
        let dir = scratch("removed");
        let (store, _ring) = store_in(&dir);
        store.move_to_keyring("work", SECRET, &dir.join("work"));
        assert!(store.names().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn adding_a_seat_never_prints_its_token() {
        let dir = scratch("add");
        let (store, _ring) = store_in(&dir);
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
