//! Secrets the user keeps in a password manager. Instead of a value, the user
//! gives a reference to an entry (`op://vault/item/field`), and Arakun keeps
//! only that. The value is read when it is needed, through the password
//! manager's own CLI, and remembered for as long as Arakun runs, so the user
//! unlocks once. [`refresh`] forgets what was read.
//!
//! 1Password (`op`) is the first [`Provider`]. Another one (Bitwarden, `pass`)
//! is a new variant with its scheme, its program and how to read with it.
//!
//! What a read prints is never logged; the log says only the stage and the
//! exit code.

use std::collections::HashMap;
use std::io::Read as _;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::secret::Secret;

/// How long a read may take. Unlocking can show a prompt the user answers.
const TIMEOUT: Duration = Duration::from_secs(45);
/// The longest reference taken.
const MAX_LEN: usize = 400;

/// A password manager Arakun reads references from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    OnePassword,
}

impl Provider {
    const ALL: [Provider; 1] = [Provider::OnePassword];

    /// What its references start with.
    pub fn scheme(self) -> &'static str {
        match self {
            Provider::OnePassword => "op://",
        }
    }

    /// Its name, as the interface says it.
    pub fn name(self) -> &'static str {
        match self {
            Provider::OnePassword => "1Password",
        }
    }

    fn program(self) -> &'static str {
        match self {
            Provider::OnePassword => "op",
        }
    }

    /// The arguments that print the value of `reference` and nothing else.
    fn read_args(self, reference: &str) -> Vec<String> {
        match self {
            // `--cache=false`: op's own cache daemon would keep the value in
            // another process after Arakun forgot it.
            Provider::OnePassword => vec![
                "read".into(),
                "--no-newline".into(),
                "--cache=false".into(),
                reference.into(),
            ],
        }
    }

    /// How to fix it when the program is not there.
    fn install_hint(self) -> &'static str {
        match self {
            Provider::OnePassword => {
                "The 1Password CLI (op) is not installed. Install it from https://developer.1password.com/docs/cli/get-started/, turn on Integrate with 1Password CLI in the 1Password app's Developer settings, then press Refresh."
            }
        }
    }

    /// The plain error for what the program wrote to stderr, which is not
    /// shown or logged as it is.
    fn explain(self, stderr: &str, reference: &str) -> String {
        let s = stderr.to_ascii_lowercase();
        match self {
            Provider::OnePassword => {
                let locked = [
                    "not currently signed in",
                    "no accounts configured",
                    "authorization prompt dismissed",
                    "authorization timeout",
                    "locked",
                    "sign in",
                    "signin",
                    "session expired",
                ];
                let missing = [
                    "isn't an item",
                    "isn't a vault",
                    "isn't a field",
                    "could not find",
                    "no item found",
                    "not found",
                    "does not have a field",
                ];
                if locked.iter().any(|w| s.contains(w)) {
                    "1Password is locked or not signed in. Unlock the 1Password app (or run `op signin` in a terminal), then press Refresh.".into()
                } else if missing.iter().any(|w| s.contains(w)) {
                    format!(
                        "1Password has no entry at {reference}. Check the vault, item and field names; Copy Secret Reference on the field in 1Password gives the right one."
                    )
                } else {
                    "1Password could not read it. Run `op read` with the reference in a terminal to see why, then press Refresh.".into()
                }
            }
        }
    }
}

/// A checked reference to one entry in a password manager.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Reference {
    provider: Provider,
    text: String,
}

impl Reference {
    /// `text` as a reference, checked strictly, since it goes on a command
    /// line: the scheme, then 3 or 4 parts of letters, digits, spaces and
    /// `_ . -`, none starting with `-`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let provider = Provider::ALL
            .into_iter()
            .find(|p| text.starts_with(p.scheme()))
            .ok_or_else(|| {
                format!(
                    "A reference starts with {}, such as op://vault/item/field.",
                    Provider::ALL.map(Provider::scheme).join(" or ")
                )
            })?;
        let bad = || {
            format!(
                "That is not a {} reference. It looks like op://vault/item/field, with letters, digits, spaces, _, . and - in each part.",
                provider.name()
            )
        };
        if text.len() > MAX_LEN {
            return Err(bad());
        }
        let rest = &text[provider.scheme().len()..];
        let parts: Vec<&str> = rest.split('/').collect();
        let part_ok = |p: &&str| {
            !p.is_empty()
                && p.len() <= 128
                && !p.starts_with(['-', ' '])
                && !p.ends_with(' ')
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '.' | '-'))
        };
        if !(3..=4).contains(&parts.len()) || !parts.iter().all(part_ok) {
            return Err(bad());
        }
        Ok(Self {
            provider,
            text: text.to_string(),
        })
    }

    pub fn provider(&self) -> Provider {
        self.provider
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

/// Whether `text` is meant as a reference: it starts with a known scheme.
pub fn looks_like_reference(text: &str) -> bool {
    let text = text.trim();
    Provider::ALL.iter().any(|p| text.starts_with(p.scheme()))
}

/// How a read went, as the program ran.
enum Ran {
    /// It was not there.
    Missing,
    /// It did not finish within [`TIMEOUT`].
    TimedOut,
    /// It exited with `code`, with what it printed.
    Exited {
        code: Option<i32>,
        stdout: Secret,
        stderr: String,
    },
    /// It could not be started or read for another reason.
    Failed,
}

/// Runs a password manager's program. The tests run a fake one.
trait Runner: Send + Sync {
    fn run(&self, program: &str, args: &[String]) -> Ran;
}

#[cfg_attr(test, allow(dead_code))]
struct Cli;

impl Runner for Cli {
    fn run(&self, program: &str, args: &[String]) -> Ran {
        let Some(path) = crate::agent::launch::which(program) else {
            return Ran::Missing;
        };
        let mut cmd = crate::platform::command(path);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ran::Missing,
            Err(_) => return Ran::Failed,
        };
        // Read the pipes on their own threads, so a full pipe never stalls
        // the child while this one waits for it to exit.
        let pipe = |r: Option<Box<dyn std::io::Read + Send>>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut r) = r {
                    let _ = r.read_to_end(&mut buf);
                }
                buf
            })
        };
        let out = pipe(child.stdout.take().map(|s| Box::new(s) as _));
        let err = pipe(child.stderr.take().map(|s| Box::new(s) as _));
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if start.elapsed() >= TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ran::TimedOut;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => return Ran::Failed,
            }
        };
        let stdout = String::from_utf8(out.join().unwrap_or_default()).unwrap_or_default();
        let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned();
        Ran::Exited {
            code: status.code(),
            stdout: Secret::new(stdout),
            stderr,
        }
    }
}

/// What was read, by reference, failures too: a locked vault is not asked
/// again and again until the user presses Refresh.
type Cache = HashMap<Reference, Result<Secret, String>>;

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
/// One read at a time, so two that want the same entry unlock it once.
static READING: Mutex<()> = Mutex::new(());

fn runner() -> &'static dyn Runner {
    #[cfg(test)]
    {
        &tests::FAKE
    }
    #[cfg(not(test))]
    {
        &Cli
    }
}

/// The value behind `reference`: from what was read before in this run, or
/// read now. Blocks for up to [`TIMEOUT`], so call it off the UI thread.
pub fn read(reference: &Reference) -> Result<Secret, String> {
    read_with(runner(), reference)
}

/// [`read`] with a reference as text, checked first.
pub fn read_text(reference: &str) -> Result<Secret, String> {
    read(&Reference::parse(reference)?)
}

fn cached(reference: &Reference) -> Option<Result<Secret, String>> {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()?
        .get(reference)
        .cloned()
}

fn read_with(runner: &dyn Runner, reference: &Reference) -> Result<Secret, String> {
    if let Some(hit) = cached(reference) {
        return hit;
    }
    let _one = READING.lock().unwrap_or_else(|e| e.into_inner());
    // Another read may have finished while this one waited.
    if let Some(hit) = cached(reference) {
        return hit;
    }
    let got = read_now(runner, reference);
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(reference.clone(), got.clone());
    got
}

fn read_now(runner: &dyn Runner, reference: &Reference) -> Result<Secret, String> {
    let p = reference.provider;
    let ran = runner.run(p.program(), &p.read_args(reference.as_str()));
    let log = |stage: &str, code: Option<i32>| {
        let code = code.map_or_else(|| "none".into(), |c| c.to_string());
        eprintln!(
            "arakun: password manager {}: {stage}, exit {code}",
            p.program()
        );
    };
    match ran {
        Ran::Missing => {
            log("not installed", None);
            Err(p.install_hint().into())
        }
        Ran::TimedOut => {
            log("timed out", None);
            Err(format!(
                "{} did not answer within {} seconds. Unlock it, then press Refresh.",
                p.name(),
                TIMEOUT.as_secs()
            ))
        }
        Ran::Failed => {
            log("could not start", None);
            Err(format!(
                "Arakun could not run {}. Check that it works in a terminal, then press Refresh.",
                p.program()
            ))
        }
        Ran::Exited {
            code: Some(0),
            stdout,
            ..
        } if !stdout.expose().is_empty() => {
            log("read", Some(0));
            Ok(stdout)
        }
        Ran::Exited { code: Some(0), .. } => {
            log("read an empty value", Some(0));
            Err(format!(
                "The field at {} is empty in {}.",
                reference.as_str(),
                p.name()
            ))
        }
        Ran::Exited { code, stderr, .. } => {
            log("failed", code);
            Err(p.explain(&stderr, reference.as_str()))
        }
    }
}

/// Reads `reference` again, past what was read before, and says whether it
/// worked. The value is not returned, so it is not shown; it is remembered
/// for the next [`read`].
pub fn check(reference: &str) -> Result<(), String> {
    let reference = Reference::parse(reference)?;
    forget(&reference);
    read(&reference).map(drop)
}

/// Forgets what was read for `reference`.
pub fn forget(reference: &Reference) {
    if let Some(cache) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        cache.remove(reference);
    }
}

/// Forgets everything read in this run, so the next read asks the password
/// manager again: after the user unlocked it, or changed an entry.
pub fn refresh() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Answers by the reference's item: `locked` and `gone` fail as op
    /// does, `slow` times out, and any other gives `value-of-<item>`.
    /// Counts the reads of each reference, as tests run side by side.
    pub(crate) struct Fake {
        calls: Mutex<Option<HashMap<String, usize>>>,
    }

    pub(crate) static FAKE: Fake = Fake {
        calls: Mutex::new(None),
    };

    impl Runner for Fake {
        fn run(&self, program: &str, args: &[String]) -> Ran {
            assert_eq!(program, "op");
            assert!(args.contains(&"--cache=false".to_string()));
            let reference = args.last().unwrap();
            *self
                .calls
                .lock()
                .unwrap()
                .get_or_insert_with(HashMap::new)
                .entry(reference.clone())
                .or_default() += 1;
            let item = reference.split('/').nth(3).unwrap_or_default();
            let exited = |code, out: &str, err: &str| Ran::Exited {
                code: Some(code),
                stdout: Secret::new(out),
                stderr: err.into(),
            };
            match item {
                "locked" => exited(
                    1,
                    "",
                    "[ERROR] 2026/11/27 You are not currently signed in. Please run `op signin --help` for instructions",
                ),
                "gone" => exited(
                    1,
                    "",
                    "[ERROR] \"gone\" isn't an item in the \"Dev\" vault.",
                ),
                "empty" => exited(0, "", ""),
                "slow" => Ran::TimedOut,
                "absent" => Ran::Missing,
                "broken" => Ran::Failed,
                other => exited(0, &format!("value-of-{other}"), ""),
            }
        }
    }

    /// Held by the tests that count reads, since [`refresh`] clears what
    /// every test read.
    pub(crate) static SERIAL: Mutex<()> = Mutex::new(());

    /// How often `reference` was read through the program.
    pub(crate) fn calls(reference: &str) -> usize {
        FAKE.calls
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|c| c.get(reference).copied())
            .unwrap_or(0)
    }

    #[test]
    fn references_are_checked_strictly() {
        for ok in [
            "op://Dev/Stripe/credential",
            " op://Private Vault/My item/section/password ",
            "op://v_1/i.2/f-3",
        ] {
            let r = Reference::parse(ok).unwrap();
            assert_eq!(r.provider(), Provider::OnePassword);
            assert_eq!(r.as_str(), ok.trim());
        }
        for bad in [
            "",
            "sk_live_123",
            "op://",
            "op://vault/item",
            "op://vault/item/a/b/c",
            "op://vault//field",
            "op://vault/-item/field",
            "op://vault/item/field; rm -rf ~",
            "op://vault/item/$(whoami)",
            "op://vault/item/field?attribute=otp",
            "op://vault/item/fi\neld",
            "op://vault/ item/field",
        ] {
            assert!(Reference::parse(bad).is_err(), "{bad:?}");
        }
        assert!(Reference::parse(&format!("op://v/i/{}", "x".repeat(400))).is_err());
        assert!(looks_like_reference(" op://x"));
        assert!(!looks_like_reference("ghp_123"));
    }

    #[test]
    fn a_read_is_remembered_until_refresh() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let r = Reference::parse("op://T/ok-remembered/field").unwrap();
        assert_eq!(read(&r).unwrap().expose(), "value-of-ok-remembered");
        assert_eq!(read(&r).unwrap().expose(), "value-of-ok-remembered");
        assert_eq!(calls(r.as_str()), 1, "the second read comes from memory");
        // Check reads again, and keeps what it read.
        check(r.as_str()).unwrap();
        assert_eq!(calls(r.as_str()), 2);
        read(&r).unwrap();
        assert_eq!(calls(r.as_str()), 2);
        refresh();
        read(&r).unwrap();
        assert_eq!(calls(r.as_str()), 3, "refresh forgets what was read");
    }

    #[test]
    fn a_failed_read_is_remembered_too() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let r = Reference::parse("op://T/locked/field").unwrap();
        let e = read(&r).unwrap_err();
        assert!(e.contains("locked or not signed in"), "{e}");
        assert!(e.contains("Refresh"), "{e}");
        forget(&r);
        let before = calls(r.as_str());
        assert!(read(&r).is_err());
        assert!(read(&r).is_err());
        assert_eq!(
            calls(r.as_str()) - before,
            1,
            "a locked vault is not asked again"
        );
    }

    #[test]
    fn errors_say_what_is_wrong_and_how_to_fix_it() {
        let e = read_text("op://T/gone/field").unwrap_err();
        assert!(e.contains("no entry at op://T/gone/field"), "{e}");
        let e = read_text("op://T/absent/field").unwrap_err();
        assert!(e.contains("not installed"), "{e}");
        assert!(e.contains("developer.1password.com"), "{e}");
        let e = read_text("op://T/slow/field").unwrap_err();
        assert!(e.contains("45 seconds"), "{e}");
        let e = read_text("op://T/broken/field").unwrap_err();
        assert!(e.contains("could not run op"), "{e}");
        let e = read_text("op://T/empty/field").unwrap_err();
        assert!(e.contains("empty"), "{e}");
        let e = read_text("not a reference").unwrap_err();
        assert!(e.contains("op://"), "{e}");
        // Nothing op wrote reaches the error as it is.
        let e = Provider::OnePassword.explain("[ERROR] sk_live_leaked", "op://a/b/c");
        assert!(!e.contains("sk_live"), "{e}");
    }
}
