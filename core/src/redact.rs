//! Redacted mode, for screen sharing, streams and demos. While it is on, the
//! window hides what identifies the user or opens something of theirs:
//! emails and account names, keys and tokens, pairing codes and addresses.
//!
//! It is kept in the settings, so a demo that starts with it on does not
//! show anything in the first frames after a restart. `ARAKUN_REDACTED=1`
//! turns it on for one run without touching the settings.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU8, Ordering};

/// What stands for a hidden character.
pub const DOT: char = '•';

/// What stands for a whole hidden value: always this long, so it does not
/// give away the length of what it hides.
pub const MASK: &str = "••••••••";

/// 0: not read yet, 1: off, 2: on.
static STATE: AtomicU8 = AtomicU8::new(0);

/// Whether the window hides personal data right now.
pub fn on() -> bool {
    match STATE.load(Ordering::Relaxed) {
        0 => {
            let on = std::env::var("ARAKUN_REDACTED").is_ok_and(|v| v == "1")
                || crate::settings::read(|s| s.privacy.redacted);
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
        s => s == 2,
    }
}

/// Turns the mode on or off and remembers it in the settings.
pub fn set(on: bool) -> crate::settings::Saving {
    STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
    crate::settings::change(|s| s.privacy.redacted = on)
}

/// `value` as it is, or [`MASK`] while the mode is on. For what is private as
/// a whole: an account's email, an account id, a key, a pairing code.
pub fn value(value: &str) -> String {
    if on() && !value.is_empty() {
        MASK.to_string()
    } else {
        value.to_string()
    }
}

/// `text` with what looks private masked while the mode is on, and as it is
/// otherwise. For free text that may hold an email or a key among other words.
pub fn text(text: &str) -> Cow<'_, str> {
    if !on() {
        return Cow::Borrowed(text);
    }
    let mut chars: Vec<char> = text.chars().collect();
    if mask(&mut chars, me()) {
        Cow::Owned(chars.into_iter().collect())
    } else {
        Cow::Borrowed(text)
    }
}

/// `text` masked as [`text`] masks it, but with `*` for each byte of a hidden
/// character, so every byte offset into it still points at the same place:
/// style runs, links, selections and search hits stay valid. `None` when the
/// mode is off or nothing needed hiding.
pub fn same_bytes(text: &str) -> Option<String> {
    if !on() {
        return None;
    }
    stars(text, me())
}

fn stars(text: &str, me: &Me) -> Option<String> {
    let mut chars: Vec<char> = text.chars().collect();
    if !mask(&mut chars, me) {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    for (was, now) in text.chars().zip(chars) {
        if was == now {
            out.push(was);
        } else {
            out.extend(std::iter::repeat_n('*', was.len_utf8()));
        }
    }
    Some(out)
}

// ---- secrets in chats ----------------------------------------------------------

/// 0: not read yet, 1: shown, 2: hidden.
static CHAT: AtomicU8 = AtomicU8::new(0);

/// Whether chats hide the secrets in them until clicked. On unless the user
/// turned it off: an agent that prints a token should not put it on screen.
pub fn hides_secrets() -> bool {
    match CHAT.load(Ordering::Relaxed) {
        0 => {
            let hides = crate::settings::read(|s| s.privacy.hide_secrets);
            CHAT.store(if hides { 2 } else { 1 }, Ordering::Relaxed);
            hides
        }
        s => s == 2,
    }
}

/// Turns hiding secrets in chats on or off and remembers it, as [`set`] does.
pub fn set_hides_secrets(hides: bool) -> crate::settings::Saving {
    CHAT.store(if hides { 2 } else { 1 }, Ordering::Relaxed);
    crate::settings::change(|s| s.privacy.hide_secrets = hides)
}

/// Secrets the user clicked to see, with how many chat views show each. A
/// view forgets what it showed when it closes.
static SHOWN: std::sync::Mutex<Vec<(String, usize)>> = std::sync::Mutex::new(Vec::new());

fn shown() -> std::sync::MutexGuard<'static, Vec<(String, usize)>> {
    SHOWN.lock().unwrap_or_else(|e| e.into_inner())
}

/// Shows `value` in chats until every view that called this calls [`forget`].
/// Does nothing in redacted mode, which shows no secret.
pub fn show(value: &str) {
    if !on() {
        count(value);
    }
}

fn count(value: &str) {
    let mut shown = shown();
    match shown.iter_mut().find(|(v, _)| v == value) {
        Some((_, n)) => *n += 1,
        None => shown.push((value.to_string(), 1)),
    }
}

/// Undoes one [`show`] of `value`.
pub fn forget(value: &str) {
    let mut shown = shown();
    if let Some(i) = shown.iter().position(|(v, _)| v == value) {
        shown[i].1 -= 1;
        if shown[i].1 == 0 {
            shown.swap_remove(i);
        }
    }
}

fn is_shown(value: &str) -> bool {
    shown().iter().any(|(v, _)| v == value)
}

/// A secret a chat hides: where, in bytes, and what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hidden {
    pub range: std::ops::Range<usize>,
    pub value: String,
}

/// Text from a chat as it shows on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Masked {
    /// The text with `*` for each byte it hides, so offsets into it stay valid.
    pub text: String,
    /// The secrets it hides. A click shows them, outside redacted mode.
    pub hidden: Vec<Hidden>,
}

/// `text` from a chat with its secrets hidden, or `None` when nothing is.
/// Redacted mode hides what [`same_bytes`] hides; otherwise, while
/// [`hides_secrets`], only the secrets the detector finds, less the ones the
/// user clicked to see. A known key keeps its first four characters.
pub fn in_chat(text: &str) -> Option<Masked> {
    if on() {
        let hidden = found(text)
            .into_iter()
            .map(|s| Hidden {
                value: text[s.range.clone()].to_string(),
                range: s.range,
            })
            .collect();
        return same_bytes(text).map(|text| Masked { text, hidden });
    }
    if !hides_secrets() {
        return None;
    }
    secrets_masked(text, is_shown)
}

/// What [`crate::secrets::find`] finds in `text`, remembered: a chat lays out
/// the same text on every frame, and the detector runs several patterns.
fn found(text: &str) -> Vec<crate::secrets::Secret> {
    // Enough for every piece on a screen or two.
    const KEEP: usize = 4096;
    thread_local! {
        static FOUND: std::cell::RefCell<std::collections::HashMap<String, Vec<crate::secrets::Secret>>> =
            Default::default();
    }
    if let Some(hit) = FOUND.with(|f| f.borrow().get(text).cloned()) {
        return hit;
    }
    let hit = crate::secrets::find(text);
    FOUND.with(|f| {
        let mut f = f.borrow_mut();
        if f.len() >= KEEP {
            f.clear();
        }
        f.insert(text.to_string(), hit.clone());
    });
    hit
}

/// `text` with the secrets in it starred, but those `shown` says to show.
fn secrets_masked(text: &str, shown: impl Fn(&str) -> bool) -> Option<Masked> {
    let found: Vec<_> = found(text)
        .into_iter()
        .filter(|s| !shown(&text[s.range.clone()]))
        .collect();
    if found.is_empty() {
        return None;
    }
    let mut out = text.to_string();
    for s in &found {
        let keep = match s.kind {
            crate::secrets::Kind::Token => text[s.range.clone()]
                .char_indices()
                .nth(4)
                .map_or(s.range.len(), |(b, _)| b),
            _ => 0,
        };
        star(&mut out, s.range.start + keep..s.range.end);
    }
    let hidden = found
        .into_iter()
        .map(|s| Hidden {
            value: text[s.range.clone()].to_string(),
            range: s.range,
        })
        .collect();
    Some(Masked { text: out, hidden })
}

/// `*` over every byte in `range`, but for line breaks: a private key keeps
/// its lines.
fn star(text: &mut String, range: std::ops::Range<usize>) {
    let starred: String = text[range.clone()]
        .chars()
        .flat_map(|c| {
            let n = if matches!(c, '\n' | '\r') {
                0
            } else {
                c.len_utf8()
            };
            std::iter::repeat_n('*', n).chain((n == 0).then_some(c))
        })
        .collect();
    text.replace_range(range, &starred);
}

/// Plain text from a chat as it shows on screen, and the secrets clicking it
/// shows. In redacted mode it is [`text`], and shows nothing.
pub fn chat_text(text: &str) -> (Cow<'_, str>, Vec<String>) {
    if on() {
        return (self::text(text), Vec::new());
    }
    match in_chat(text) {
        Some(m) => (
            Cow::Owned(m.text),
            m.hidden.into_iter().map(|h| h.value).collect(),
        ),
        None => (Cow::Borrowed(text), Vec::new()),
    }
}

/// Whether copying `text` out of a chat would copy a secret the chat hides,
/// so copying asks first.
pub fn hides_in_chat(text: &str) -> bool {
    in_chat(text).is_some_and(|m| !m.hidden.is_empty())
}

/// Who the user is, as text on screen shows it.
pub struct Me {
    /// The home folder, the user's name being its last part.
    home: String,
    /// Words that name the user or the machine: the user name and the host
    /// name, as a shell prompt shows them (`ann@ann-laptop`).
    names: Vec<String>,
}

impl Me {
    fn new(home: &str, host: Option<&str>) -> Self {
        let user = home
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default();
        let mut names = vec![user.to_string()];
        if let Some(host) = host {
            names.push(host.to_string());
            // A prompt shows `ann-laptop`, the machine knows `ann-laptop.lan`.
            if let Some((short, _)) = host.split_once('.') {
                names.push(short.to_string());
            }
        }
        // Two letters would mask too many words that only look like them.
        names.retain(|n| n.chars().count() >= 3);
        Self {
            home: home.to_string(),
            names,
        }
    }
}

fn me() -> &'static Me {
    static ME: std::sync::OnceLock<Me> = std::sync::OnceLock::new();
    ME.get_or_init(|| {
        let home = crate::platform::home().to_string_lossy().into_owned();
        Me::new(&home, host_name().as_deref())
    })
}

#[cfg(unix)]
fn host_name() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its whole length, which is what we pass.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if r != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..end].to_vec()).ok()
}

#[cfg(not(unix))]
fn host_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

/// Masks, in place, what looks private in `chars`: the secrets
/// [`crate::secrets::find`] finds, emails, IPv4 addresses, the user's name
/// in the home folder, and the user and host names as words
/// (`ann@ann-laptop`). Every hidden character becomes one [`DOT`], so the
/// text keeps its width: a terminal's columns stay where they were. A key
/// known by its shape keeps its first four characters, which say what it is.
/// Returns whether it masked anything.
pub fn mask(chars: &mut [char], me: &Me) -> bool {
    // Secrets first, on the text as it is: dots would break their shapes.
    let mut changed = mask_secrets(chars);
    changed |= mask_home(chars, &me.home);
    let mut i = 0;
    while i < chars.len() {
        if !is_word(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_word(chars[i]) {
            i += 1;
        }
        let word: String = chars[start..i].iter().collect();
        if let Some(at) = word.find('@').filter(|_| is_email(&word)) {
            // A full stop after the address ends the sentence, not the address.
            let end = i - (word.len() - word.trim_end_matches('.').len());
            let at = start + word[..at].chars().count();
            changed |= dot(&mut chars[start..at], 0);
            changed |= dot(&mut chars[at + 1..end], 0);
        } else if is_public_ipv4(&word) {
            for c in &mut chars[start..i] {
                if *c != '.' {
                    *c = DOT;
                    changed = true;
                }
            }
        } else {
            // `ann@ann-laptop`: each side on its own.
            let mut at = start;
            for part in word.split('@') {
                let len = part.chars().count();
                let bare = part.trim_end_matches('.');
                if me.names.iter().any(|n| n == bare) {
                    changed |= dot(&mut chars[at..at + bare.chars().count()], 0);
                }
                at += len + 1;
            }
        }
    }
    changed
}

/// Dots over the secrets in `chars`; whether there were any.
fn mask_secrets(chars: &mut [char]) -> bool {
    let text: String = chars.iter().collect();
    let found = crate::secrets::find(&text);
    if found.is_empty() {
        return false;
    }
    // Where each character starts in `text`, to turn bytes into characters.
    let starts: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    let index = |byte: usize| starts.partition_point(|&s| s < byte);
    let mut changed = false;
    for s in found {
        let keep = if s.kind == crate::secrets::Kind::Token {
            4
        } else {
            0
        };
        changed |= dot(&mut chars[index(s.range.start)..index(s.range.end)], keep);
    }
    changed
}

/// Characters a word, an email or a token is made of.
fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+' | '@' | '%')
}

/// Dots over `chars` past the first `keep`; whether there was any to cover.
fn dot(chars: &mut [char], keep: usize) -> bool {
    let mut changed = false;
    for c in chars.iter_mut().skip(keep) {
        // A private key spans lines; the lines stay.
        if *c != DOT && *c != '\n' && *c != '\r' {
            *c = DOT;
            changed = true;
        }
    }
    changed
}

/// Masks the user's name wherever the home folder shows, `/home/ann/x`
/// becoming `/home/•••/x`.
fn mask_home(chars: &mut [char], home: &str) -> bool {
    let home: Vec<char> = home.trim_end_matches(['/', '\\']).chars().collect();
    let name = home
        .iter()
        .rev()
        .take_while(|c| !matches!(c, '/' | '\\'))
        .count();
    // `/` or `C:\` alone has no name to hide.
    if name == 0 || name == home.len() || chars.len() < home.len() {
        return false;
    }
    let mut changed = false;
    let mut i = 0;
    while i + home.len() <= chars.len() {
        if chars[i..i + home.len()] == home[..] {
            changed |= dot(&mut chars[i + home.len() - name..i + home.len()], 0);
            i += home.len();
        } else {
            i += 1;
        }
    }
    changed
}

fn is_email(word: &str) -> bool {
    let Some((user, domain)) = word.split_once('@') else {
        return false;
    };
    let domain = domain.trim_end_matches('.');
    // `git@github.com:o/r` is a remote, the same for everyone.
    !user.is_empty()
        && user != "git"
        && !domain.contains('@')
        && domain.rsplit_once('.').is_some_and(|(name, tld)| {
            !name.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
        })
}

/// An IPv4 address other than the loopback and `0.0.0.0`, which say nothing
/// about where the user is.
fn is_public_ipv4(word: &str) -> bool {
    let Ok(ip) = word.parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    !ip.is_loopback() && !ip.is_unspecified()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/ann";

    fn ann() -> Me {
        Me::new(HOME, Some("ann-box.lan"))
    }

    fn masked(s: &str) -> String {
        let mut chars: Vec<char> = s.chars().collect();
        mask(&mut chars, &ann());
        chars.into_iter().collect()
    }

    #[test]
    fn plain_text_stays() {
        for s in [
            "cargo test --locked",
            "fix(terminal): keep the tab name on resize",
            "commit 840ea5e1b0f3c2a9d8e7f6a5b4c3d2e1f0a9b8c7",
            "npx @playwright/mcp@latest --viewport-size 1280x800",
            "listening on 127.0.0.1:3000 and 0.0.0.0:8080",
            "KEYBOARD=us SSH_KEY_PATH=/x PWD=/tmp",
            "git clone git@github.com:o/r.git",
            "version 0.2.1",
        ] {
            assert_eq!(masked(s), s, "{s}");
        }
    }

    #[test]
    fn hides_emails_but_keeps_their_shape() {
        assert_eq!(
            masked("signed in as ann@example.com."),
            "signed in as •••@•••••••••••."
        );
    }

    #[test]
    fn keeps_the_start_of_a_key() {
        let key = "sk-ant-api03-FAKEFAKEFAKE1234";
        assert_eq!(
            masked(&format!("token {key}")),
            format!("token sk-a{}", "•".repeat(key.len() - 4))
        );
        let random = "Abc123Def456Ghi789Jkl012Mno";
        assert_eq!(
            masked(&format!("x {random} x")),
            format!("x Abc1{} x", "•".repeat(random.len() - 4))
        );
    }

    #[test]
    fn hides_values_of_secret_names() {
        assert_eq!(
            masked("OPENAI_API_KEY=abc/def=="),
            "OPENAI_API_KEY=•••••••••"
        );
        assert_eq!(masked("\"token\": \"hunter2\","), "\"token\": \"•••••••\",");
        assert_eq!(
            masked("Authorization: Bearer abc.def"),
            "Authorization: Bearer •••••••"
        );
    }

    #[test]
    fn hides_private_keys_and_passwords_in_urls() {
        assert_eq!(
            masked("postgres://app:s3cr3t@db/app"),
            "postgres://app:••••••@db/app"
        );
        let key = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----";
        let want: String = key
            .chars()
            .map(|c| if c == '\n' { c } else { DOT })
            .collect();
        assert_eq!(masked(key), want);
    }

    #[test]
    fn leaves_comparisons_alone() {
        let s = "if event.key == \"Enter\" {";
        assert_eq!(masked(s), s);
    }

    #[test]
    fn hides_the_user_name_and_addresses() {
        assert_eq!(masked("cd /home/ann/code"), "cd /home/•••/code");
        assert_eq!(masked("ssh 203.0.113.7"), "ssh •••.•.•••.•");
        // `/` as home has no name in it.
        let mut chars: Vec<char> = "/etc".chars().collect();
        assert!(!mask(&mut chars, &Me::new("/", None)));
    }

    #[test]
    fn hides_the_user_and_the_machine_in_a_prompt() {
        assert_eq!(masked("ann@ann-box:~$ ls"), "•••@•••••••:~$ ls");
        assert_eq!(masked("ssh ann-box.lan."), "ssh •••••••••••.");
        assert_eq!(masked("Annotate the annex"), "Annotate the annex");
    }

    #[test]
    fn keeps_the_width() {
        let s = "ann@example.com sk-proj-0123456789abcdefABCDEF /home/ann 10.1.2.3 é";
        assert_eq!(masked(s).chars().count(), s.chars().count());
    }

    #[test]
    fn stars_keep_every_byte_offset() {
        let s = "a /home/ann/x bo@xy.de ok";
        assert_eq!(stars(s, &ann()).unwrap(), "a /home/***/x **@***** ok");
        // `ö` is two bytes, so two stars.
        let s = "/home/jöe/x";
        let out = stars(s, &Me::new("/home/jöe", None)).unwrap();
        assert_eq!(out, "/home/****/x");
        assert_eq!(out.len(), s.len());
        assert_eq!(stars("plain words", &ann()), None);
    }

    #[test]
    fn a_chat_hides_only_secrets_and_keeps_offsets() {
        let none = |_: &str| false;
        let s = "ann@example.com set OPENAI_API_KEY=abc/def== and sk-ant-api03-FAKEFAKEFAKE1234";
        let m = secrets_masked(s, none).unwrap();
        assert_eq!(m.text.len(), s.len());
        let key = "sk-ant-api03-FAKEFAKEFAKE1234";
        assert_eq!(
            m.text,
            format!(
                "ann@example.com set OPENAI_API_KEY=********* and sk-a{}",
                "*".repeat(key.len() - 4)
            )
        );
        let values: Vec<&str> = m.hidden.iter().map(|h| h.value.as_str()).collect();
        assert_eq!(values, ["abc/def==", key]);
        for h in &m.hidden {
            assert_eq!(&s[h.range.clone()], h.value);
        }
        assert_eq!(secrets_masked("cd /home/ann 10.1.2.3", none), None);
    }

    #[test]
    fn a_chat_shows_what_was_clicked() {
        let s = "a=1 TOKEN=hunter22 and DB_PASSWORD=s3cr3t";
        let m = secrets_masked(s, |v| v == "hunter22").unwrap();
        assert_eq!(m.text, "a=1 TOKEN=hunter22 and DB_PASSWORD=******");
        assert_eq!(m.hidden.len(), 1);
        assert_eq!(secrets_masked(s, |_| true), None);
    }

    #[test]
    fn a_private_key_keeps_its_lines() {
        let key = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----";
        let m = secrets_masked(key, |_| false).unwrap();
        assert_eq!(m.text.lines().count(), 3);
        assert!(m.text.chars().all(|c| c == '*' || c == '\n'));
    }

    #[test]
    fn shown_values_are_counted_per_view() {
        let v = "count-me-0f3e9a";
        count(v);
        count(v);
        forget(v);
        assert!(is_shown(v));
        forget(v);
        assert!(!is_shown(v));
    }
}
