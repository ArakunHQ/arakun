//! Where Arakun keeps its own secrets: API keys, tokens and private keys.
//!
//! The OS keyring holds them: the Keychain, the Credential Manager or the
//! Secret Service. Where there is no keyring (a headless Linux box with no
//! Secret Service), a private file in the data folder holds them instead, and
//! the person is told once. The JSON files next to them keep only what is not
//! secret: names, hosts and URLs.
//!
//! Finding secrets in text is [`crate::secrets`]; this is keeping them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use crate::platform::keyring::{self, Error};

/// The longest piece one keyring entry holds. The Windows Credential Manager
/// takes at most 2560 bytes, and OAuth tokens and keys can be longer, so a
/// longer secret is split into parts on every platform. The same code then
/// runs, and is tested, everywhere.
const PART: usize = 2048;

/// What the first entry of a split secret holds, before the number of parts.
const PARTS: &str = "arakun-parts:";

/// What the person is told the first time a secret goes to the file.
pub const FALLBACK_NOTICE: &str = "This computer has no keyring, so Arakun keeps its keys and tokens in a private file only you can read. Install a Secret Service, such as GNOME Keyring, to keep them in a keyring.";

/// The secret filed under `account`. None means there is none. An error means
/// the keyring could not say (it stayed locked, or its prompt was dismissed),
/// so a locked keyring never looks like a first run.
pub fn get(account: &str) -> Result<Option<String>, String> {
    os().get(account)
}

/// Keeps `secret` under `account`, replacing what was there. `label` is the
/// name the keyring's own app shows ("arakun: Linear API key").
pub fn set(account: &str, label: &str, secret: &str) -> Result<(), String> {
    os().set(account, label, secret)
}

/// Removes the secret under `account`, from the keyring and from the file.
pub fn forget(account: &str) {
    os().forget(account)
}

/// [`FALLBACK_NOTICE`], the first time a secret went to the file instead of a
/// keyring, and None after that. The app shows it as a notice.
pub fn take_notice() -> Option<&'static str> {
    NOTICE
        .swap(false, Ordering::Relaxed)
        .then_some(FALLBACK_NOTICE)
}

static NOTICE: AtomicBool = AtomicBool::new(false);

/// The keyring account for `name`. A test Arakun with a state file of its own
/// (`ARAKUN_STATE`, as `arakun dev` sets) keeps its own secrets, so it never
/// signs the person's own Arakun out or takes its keys. Only for a store
/// whose file is next to the state too: a store whose file every profile
/// shares uses `name` as it is, or a test Arakun would move the person's own
/// secret to an entry only it finds.
pub fn account(name: &str) -> String {
    account_in(name, std::env::var_os("ARAKUN_STATE").as_deref())
}

fn account_in(name: &str, state: Option<&std::ffi::OsStr>) -> String {
    // A path that is not UTF-8 still names a test Arakun. Read as unset, it
    // would give that one the person's own secrets to use and to clear.
    let state = state.map(|s| s.to_string_lossy());
    account_for(name, state.as_deref().map(profile).as_deref())
}

/// `ARAKUN_STATE` as it names this profile's credentials. A relative path
/// is read from the working directory, like the state file itself, so two
/// folders with `state.json` don't share one device and session. An
/// absolute path stays as given, so the names already in use don't change.
pub fn profile(state: &str) -> String {
    let path = Path::new(state);
    if path.is_absolute() {
        return state.to_string();
    }
    std::path::absolute(path)
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| state.to_string())
}

/// [`account`] for a profile named by `state`, a path as [`profile`] gives it.
pub fn account_for(name: &str, state: Option<&str>) -> String {
    match state.filter(|s| !s.is_empty()) {
        Some(state) => format!("{name}@{state}"),
        None => name.to_string(),
    }
}

/// The file secrets go to when there is no keyring. One for every profile:
/// a store whose own file is shared names its secrets the same in every
/// profile, and [`account`] tells the others apart.
fn fallback_path() -> PathBuf {
    crate::paths::shared("secrets.json")
}

fn os() -> Store<'static, Os> {
    Store {
        ring: Os,
        file: fallback_path(),
        notice: &NOTICE,
    }
}

/// One process reads and rewrites the fallback file at a time.
static FILE: Mutex<()> = Mutex::new(());

/// A keyring: the OS's own, or a fake one in the tests.
trait Ring {
    fn read(&self, account: &str) -> Result<Option<String>, Error>;
    fn store(&self, account: &str, label: &str, secret: &str) -> Result<(), Error>;
    fn clear(&self, account: &str);
}

struct Os;

impl Ring for Os {
    fn read(&self, account: &str) -> Result<Option<String>, Error> {
        keyring::read(account)
    }

    fn store(&self, account: &str, label: &str, secret: &str) -> Result<(), Error> {
        keyring::store(account, label, secret)
    }

    fn clear(&self, account: &str) {
        keyring::clear(account)
    }
}

/// What the fallback file holds.
#[derive(Default, Serialize, Deserialize)]
struct File {
    /// Whether the person was told secrets are kept here.
    #[serde(default)]
    told: bool,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

struct Store<'a, R> {
    ring: R,
    file: PathBuf,
    notice: &'a AtomicBool,
}

impl<R: Ring> Store<'_, R> {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        let kept = match self.read_ring(account) {
            Ok(Some(secret)) => return Ok(Some(secret)),
            Ok(None) => self.load().secrets.remove(account),
            Err(Error::Unavailable(_)) => return Ok(self.load().secrets.remove(account)),
            // A secret kept in the file while there was no keyring is still
            // there when the keyring is back but locked.
            Err(e @ Error::Failed(_)) => {
                return match self.load().secrets.remove(account) {
                    Some(secret) => Ok(Some(secret)),
                    None => Err(e.to_string()),
                };
            }
        };
        // A keyring that is here now takes over what the file kept while it
        // was not.
        if let Some(secret) = &kept
            && self
                .store_ring(account, &format!("arakun: {account}"), secret)
                .is_ok()
        {
            self.drop_from_file(account);
        }
        Ok(kept)
    }

    fn set(&self, account: &str, label: &str, secret: &str) -> Result<(), String> {
        match self.store_ring(account, label, secret) {
            Ok(()) => {
                self.drop_from_file(account);
                Ok(())
            }
            Err(Error::Unavailable(why)) => {
                eprintln!("arakun: no keyring ({why}), keeping {account} in a private file");
                let mut tell = false;
                self.edit(|f| {
                    tell = !f.told;
                    f.told = true;
                    f.secrets.insert(account.to_string(), secret.to_string());
                    true
                })?;
                if tell {
                    self.notice.store(true, Ordering::Relaxed);
                }
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn forget(&self, account: &str) {
        self.clear_ring(account);
        self.drop_from_file(account);
    }

    /// Takes `account` out of the fallback file. The keyring wins on a read,
    /// so a copy that could not be removed is only logged.
    fn drop_from_file(&self, account: &str) {
        if let Err(e) = self.edit(|f| f.secrets.remove(account).is_some()) {
            eprintln!("arakun: {e}");
        }
    }

    /// The secret as the keyring holds it, put back together when it was split.
    fn read_ring(&self, account: &str) -> Result<Option<String>, Error> {
        let Some(first) = self.ring.read(account)? else {
            return Ok(None);
        };
        let Some(n) = parts(&first) else {
            return Ok(Some(first));
        };
        let mut secret = String::new();
        for i in 1..=n {
            match self.ring.read(&part_account(account, i))? {
                Some(part) => secret.push_str(&part),
                None => return Err(Error::Failed(format!("part {i} of {n} is missing"))),
            }
        }
        Ok(Some(secret))
    }

    fn store_ring(&self, account: &str, label: &str, secret: &str) -> Result<(), Error> {
        let before = self
            .ring
            .read(account)
            .ok()
            .flatten()
            .and_then(|s| parts(&s));
        let pieces = split(secret);
        if pieces.len() > 1 {
            for (i, piece) in pieces.iter().enumerate() {
                self.ring
                    .store(&part_account(account, i + 1), label, piece)?;
            }
            self.ring
                .store(account, label, &format!("{PARTS}{}", pieces.len()))?;
        } else {
            self.ring.store(account, label, secret)?;
        }
        // The parts a longer secret left behind.
        let now = if pieces.len() > 1 { pieces.len() } else { 0 };
        for i in now + 1..=before.unwrap_or(0) {
            self.ring.clear(&part_account(account, i));
        }
        Ok(())
    }

    fn clear_ring(&self, account: &str) {
        if let Some(n) = self
            .ring
            .read(account)
            .ok()
            .flatten()
            .and_then(|s| parts(&s))
        {
            for i in 1..=n {
                self.ring.clear(&part_account(account, i));
            }
        }
        self.ring.clear(account);
    }

    fn load(&self) -> File {
        let _held = FILE.lock().unwrap_or_else(|e| e.into_inner());
        self.load_held()
    }

    fn load_held(&self) -> File {
        crate::durable::load_json(&self.file).unwrap_or_default()
    }

    /// Changes the file, and writes it only when `change` says it changed, so a
    /// box with a keyring never gets one.
    fn edit(&self, change: impl FnOnce(&mut File) -> bool) -> Result<(), String> {
        let _held = FILE.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = self.load_held();
        if !change(&mut file) {
            return Ok(());
        }
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
        crate::durable::write_private(&self.file, &bytes)
            .map_err(|e| format!("Could not save {}: {e}", self.file.display()))
    }
}

/// How many parts a split secret's first entry says it has.
fn parts(first: &str) -> Option<usize> {
    first.strip_prefix(PARTS)?.parse().ok().filter(|n| *n > 0)
}

fn part_account(account: &str, i: usize) -> String {
    format!("{account}#{i}")
}

/// `secret` in pieces of at most [`PART`] bytes, cut between characters.
fn split(secret: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut rest = secret;
    while rest.len() > PART {
        let mut at = PART;
        while !rest.is_char_boundary(at) {
            at -= 1;
        }
        let (piece, tail) = rest.split_at(at);
        pieces.push(piece);
        rest = tail;
    }
    pieces.push(rest);
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A keyring in memory. `missing` makes it act like a box with none.
    #[derive(Default)]
    struct Fake {
        entries: RefCell<HashMap<String, String>>,
        missing: bool,
        locked: bool,
    }

    impl Ring for &Fake {
        fn read(&self, account: &str) -> Result<Option<String>, Error> {
            if self.missing {
                return Err(Error::Unavailable("no Secret Service".into()));
            }
            if self.locked {
                return Err(Error::Failed("locked".into()));
            }
            Ok(self.entries.borrow().get(account).cloned())
        }

        fn store(&self, account: &str, _label: &str, secret: &str) -> Result<(), Error> {
            if self.missing {
                return Err(Error::Unavailable("no Secret Service".into()));
            }
            if self.locked {
                return Err(Error::Failed("locked".into()));
            }
            if secret.len() > 2560 {
                return Err(Error::Failed("too long".into()));
            }
            self.entries
                .borrow_mut()
                .insert(account.to_string(), secret.to_string());
            Ok(())
        }

        fn clear(&self, account: &str) {
            self.entries.borrow_mut().remove(account);
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("arakun-secret-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("secrets.json")
    }

    fn store<'a>(ring: &'a Fake, file: &Path, notice: &'a AtomicBool) -> Store<'a, &'a Fake> {
        Store {
            ring,
            file: file.to_path_buf(),
            notice,
        }
    }

    #[test]
    fn keeps_reads_and_forgets_a_secret_in_the_keyring() {
        let (ring, file, notice) = (Fake::default(), scratch("ring"), AtomicBool::new(false));
        let s = store(&ring, &file, &notice);
        assert_eq!(s.get("linear").unwrap(), None);
        s.set("linear", "arakun: Linear", "lin_api_1").unwrap();
        assert_eq!(s.get("linear").unwrap().as_deref(), Some("lin_api_1"));
        s.set("linear", "arakun: Linear", "lin_api_2").unwrap();
        assert_eq!(s.get("linear").unwrap().as_deref(), Some("lin_api_2"));
        s.forget("linear");
        assert_eq!(s.get("linear").unwrap(), None);
        assert!(!file.exists(), "the keyring was there, so no file");
        assert!(!notice.load(Ordering::Relaxed));
    }

    #[test]
    fn a_long_secret_is_split_and_put_back_together() {
        let (ring, file, notice) = (Fake::default(), scratch("long"), AtomicBool::new(false));
        let s = store(&ring, &file, &notice);
        let long = "é".repeat(3000);
        s.set("doop", "arakun: Doop", &long).unwrap();
        assert_eq!(ring.entries.borrow()["doop"], "arakun-parts:3");
        assert!(ring.entries.borrow().values().all(|v| v.len() <= PART));
        assert_eq!(s.get("doop").unwrap().as_deref(), Some(long.as_str()));
        // A shorter secret leaves no parts of the longer one behind.
        s.set("doop", "arakun: Doop", "short").unwrap();
        assert_eq!(ring.entries.borrow().len(), 1);
        assert_eq!(s.get("doop").unwrap().as_deref(), Some("short"));
        s.set("doop", "arakun: Doop", &long).unwrap();
        s.forget("doop");
        assert!(ring.entries.borrow().is_empty());
    }

    #[test]
    fn a_split_secret_with_a_part_missing_is_an_error() {
        let (ring, file, notice) = (Fake::default(), scratch("part"), AtomicBool::new(false));
        let s = store(&ring, &file, &notice);
        s.set("doop", "arakun: Doop", &"x".repeat(5000)).unwrap();
        ring.entries.borrow_mut().remove("doop#2");
        assert!(s.get("doop").is_err());
    }

    #[test]
    fn without_a_keyring_a_private_file_keeps_it_and_the_person_is_told_once() {
        let ring = Fake {
            missing: true,
            ..Fake::default()
        };
        let (file, notice) = (scratch("file"), AtomicBool::new(false));
        let s = store(&ring, &file, &notice);
        s.set("posthog", "arakun: PostHog", "phx_1").unwrap();
        assert!(notice.swap(false, Ordering::Relaxed));
        s.set("linear", "arakun: Linear", "lin_api_1").unwrap();
        assert!(!notice.load(Ordering::Relaxed), "told only once");
        assert_eq!(s.get("posthog").unwrap().as_deref(), Some("phx_1"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        s.forget("posthog");
        assert_eq!(s.get("posthog").unwrap(), None);
        assert_eq!(s.get("linear").unwrap().as_deref(), Some("lin_api_1"));
    }

    #[test]
    fn a_keyring_that_comes_back_takes_the_secret_from_the_file() {
        let (file, notice) = (scratch("back"), AtomicBool::new(false));
        let none = Fake {
            missing: true,
            ..Fake::default()
        };
        store(&none, &file, &notice)
            .set("seat", "arakun: seat", "tok")
            .unwrap();
        let ring = Fake::default();
        let s = store(&ring, &file, &notice);
        assert_eq!(s.get("seat").unwrap().as_deref(), Some("tok"));
        assert_eq!(ring.entries.borrow()["seat"], "tok");
        let left: File = crate::durable::load_json(&file).unwrap();
        assert!(left.secrets.is_empty());
        assert!(left.told, "a later fallback is not told again");
    }

    #[test]
    fn a_locked_keyring_is_an_error_not_a_first_run() {
        let ring = Fake {
            locked: true,
            ..Fake::default()
        };
        let (file, notice) = (scratch("locked"), AtomicBool::new(false));
        let s = store(&ring, &file, &notice);
        assert!(s.get("linear").is_err());
        assert!(s.set("linear", "arakun: Linear", "x").is_err());
        assert!(!file.exists(), "a locked keyring is not a missing one");
    }

    #[test]
    fn split_cuts_between_characters() {
        assert_eq!(split("abc"), vec!["abc"]);
        assert_eq!(split(""), vec![""]);
        let s = "€".repeat(PART);
        let pieces = split(&s);
        assert!(pieces.iter().all(|p| p.len() <= PART));
        assert_eq!(pieces.concat(), s);
    }

    #[test]
    fn a_test_arakun_keeps_its_own_keyring_entries() {
        assert_eq!(account_for("teams-session", None), "teams-session");
        assert_eq!(account_for("teams-session", Some("")), "teams-session");
        assert_eq!(
            account_for("teams-device", Some("/tmp/dev/state.json")),
            "teams-device@/tmp/dev/state.json"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_state_path_that_is_not_utf8_still_keeps_its_own_entries() {
        use std::os::unix::ffi::OsStrExt as _;
        let state = std::ffi::OsStr::from_bytes(b"/tmp/dev\xff/state.json");
        assert!(state.to_str().is_none());
        assert_eq!(
            account_in("teams-session", Some(state)),
            "teams-session@/tmp/dev\u{fffd}/state.json"
        );
        assert_eq!(account_in("teams-session", None), "teams-session");
    }

    #[test]
    fn a_relative_state_names_its_own_folder() {
        let absolute = std::env::temp_dir().join("dev").join("state.json");
        let absolute = absolute.to_str().unwrap();
        assert_eq!(profile(absolute), absolute);
        let here = std::env::current_dir().unwrap().join("state.json");
        assert_eq!(profile("state.json"), here.to_str().unwrap());
        assert_eq!(profile(""), "");
    }
}
