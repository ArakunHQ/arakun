//! What keeps secrets out of @bot's reads and answers.
//!
//! The chat steers the run, and anyone in the channel can write in it. So a
//! member could ask the run to quote a key file, and the answer goes to the
//! whole channel. Two guards stand in the way, and neither depends on the
//! model following its prompt:
//!
//! - The run never sees the project itself, only a private copy of it
//!   ([`mirror`]) without secret-looking files ([`is_secret_path`]) and
//!   without what git ignores. Claude Code reads only inside its working
//!   folder, so those files are not there to read. The keys and passwords
//!   in the files it does copy are replaced with `[redacted]` too, so the
//!   chat cannot have one posted spelled out, reversed or encoded, past the
//!   check on the answer.
//! - The answer goes through [`redact`] before it is posted, which replaces
//!   keys, tokens and passwords with `[redacted]`.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use regex::{Captures, Regex};

use super::bot::Answer;

/// What a secret is replaced with in the answer.
pub const REDACTED: &str = "[redacted]";

/// How the folders of the run's copies start, in the temp folder.
pub const COPY_PREFIX: &str = "arakun-bot-";

/// A file larger than this is left out of the copy: it is rarely code, and a
/// few would fill the copy.
const FILE_BYTES: u64 = 1024 * 1024;
/// The copy stops growing here; the files past it are left out.
const TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// The copy holds at most this many files.
const FILES: usize = 50_000;

/// Folders whose files are all secrets.
const SECRET_DIRS: &[&str] = &[".ssh", ".gnupg", ".aws"];
/// File names that hold credentials.
const SECRET_NAMES: &[&str] = &[".netrc", ".npmrc", ".pypirc", ".git-credentials"];
/// SSH key files, as `ssh-keygen` names them. A name that goes on from one
/// with `_` or `.` (`id_rsa_github`, `id_ed25519.old`) is a key as well.
const SSH_KEYS: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];
/// Endings of key and certificate store files.
const SECRET_EXTENSIONS: &[&str] = &["pem", "key", "p12", "pfx", "jks", "keystore"];

/// Whether a path in the project looks like it holds secrets: `.env`,
/// `.env.*`, `.env-*`, `.env_*`, `.envrc*` and `.envs/`, `*.pem`, `*.key`
/// and other key stores, SSH keys (`id_rsa`, `id_rsa_github`,
/// `id_ed25519.pub`), credential files, and anything under `.ssh/`,
/// `.gnupg/` or `.aws/`. Case doesn't matter.
pub fn is_secret_path(rel: &Path) -> bool {
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect();
    let Some((name, dirs)) = parts.split_last() else {
        return false;
    };
    // Only the names these tools use, and what goes on from them:
    // `id_token.rs` or `.environment/` is code, and leaving it out would
    // make @bot say it does not exist.
    let env = |n: &str| {
        n == ".env"
            || n == ".envs"
            || n.starts_with(".envrc")
            || [".env.", ".env-", ".env_"].iter().any(|p| n.starts_with(p))
    };
    let ssh = |n: &str| {
        SSH_KEYS.iter().any(|k| {
            n.strip_prefix(k)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(['_', '.']))
        })
    };
    if dirs
        .iter()
        .any(|d| SECRET_DIRS.contains(&d.as_str()) || env(d))
    {
        return true;
    }
    env(name)
        || ssh(name)
        || SECRET_NAMES.contains(&name.as_str())
        || Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| SECRET_EXTENSIONS.contains(&e))
}

/// The copy of a project the run reads.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Mirror {
    /// Files copied.
    pub files: usize,
    /// Files left out for their size or past the copy's limits. Secret and
    /// ignored files are not counted: the run is not meant to know of them.
    pub too_big: usize,
    /// Files that could not be read or written, as when the temp folder is
    /// full.
    pub failed: usize,
    /// Submodules and nested repositories, which git lists as one folder
    /// and the copy leaves out.
    pub nested: Vec<PathBuf>,
}

impl Mirror {
    /// What the answer says under "not checked" about what the copy left
    /// out, so the run is not taken to have read the whole project.
    pub fn notes(&self) -> Vec<String> {
        let files = |n: usize| {
            if n == 1 {
                "1 file of the project".to_string()
            } else {
                format!("{n} files of the project")
            }
        };
        let mut notes = Vec::new();
        if self.too_big > 0 {
            let verb = if self.too_big == 1 { "was" } else { "were" };
            notes.push(format!("{} {verb} too large to read.", files(self.too_big)));
        }
        if self.failed > 0 {
            notes.push(format!(
                "{} could not be copied to read.",
                files(self.failed)
            ));
        }
        if !self.nested.is_empty() {
            let names: Vec<String> = self
                .nested
                .iter()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .collect();
            notes.push(format!(
                "Submodules and nested repositories were not read: {}.",
                names.join(", ")
            ));
        }
        notes
    }
}

/// Copies `project` into the empty folder `into`, without secret-looking
/// files and, in a git checkout, without what git ignores. Symbolic links
/// are left out, so nothing in the copy leads outside it. Text files go
/// through [`redact`] on the way: the chat can ask for a key in a form the
/// answer's check does not know, spelled out or reversed, so the run must
/// not see it at all. Fails with "stopped" once `stop` is set.
pub fn mirror(project: &Path, into: &Path, stop: &AtomicBool) -> Result<Mirror, String> {
    let root = crate::platform::canonicalize(project)
        .map_err(|e| format!("could not open the project: {e}"))?;
    let mut out = Mirror::default();
    let files = match git_files(&root) {
        Some(files) => files,
        None => {
            let (files, past) = walk_files(&root, stop);
            out.too_big += past;
            files
        }
    };
    let mut total = 0u64;
    for rel in files {
        if stop.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        if is_secret_path(&rel) {
            continue;
        }
        let from = root.join(&rel);
        // Not following links: a link in the project can point anywhere.
        let Ok(meta) = std::fs::symlink_metadata(&from) else {
            continue;
        };
        if meta.is_dir() {
            // Git lists a submodule or a nested repository as its folder.
            out.nested.push(rel);
            continue;
        }
        if !meta.file_type().is_file() {
            continue;
        }
        if meta.len() > FILE_BYTES || total + meta.len() > TOTAL_BYTES || out.files >= FILES {
            out.too_big += 1;
            continue;
        }
        if copy_redacted(&from, &into.join(&rel)).is_ok() {
            total += meta.len();
            out.files += 1;
        } else {
            out.failed += 1;
        }
    }
    Ok(out)
}

/// Copies one file, with the secrets in a text file replaced.
fn copy_redacted(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let bytes = std::fs::read(from)?;
    match std::str::from_utf8(&bytes) {
        Ok(text) => std::fs::write(to, redact(text)),
        Err(_) => std::fs::write(to, &bytes),
    }
}

/// Removes copies a run left behind when Arakun quit in the middle of it.
/// Only those a day old: another Arakun may be running one now.
pub fn sweep_copies() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    for e in entries.flatten() {
        let old = e
            .metadata()
            .ok()
            .filter(|m| m.is_dir())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > day);
        if old && e.file_name().to_string_lossy().starts_with(COPY_PREFIX) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// The files of a git checkout, relative to `root`: tracked and untracked,
/// minus everything git ignores, tracked or not. `None` outside git.
fn git_files(root: &Path) -> Option<Vec<PathBuf>> {
    let list = |extra: &[&str]| -> Option<Vec<PathBuf>> {
        let mut args = vec!["ls-files", "-z", "--exclude-standard"];
        args.extend_from_slice(extra);
        // `git::git` drops a GIT_DIR inherited from whatever started Arakun,
        // which would list another repository's files and ignore rules.
        let out = crate::platform::output_within(
            crate::git::git(root).args(&args),
            None,
            crate::platform::GIT_LOCAL,
        )
        .ok()
        .filter(|o| o.status.success())?;
        Some(split_z(&out.stdout))
    };
    let all = list(&["--cached", "--others"])?;
    // A tracked file can match an ignore rule too; leave it out as well.
    let ignored: HashSet<PathBuf> = list(&["--cached", "--ignored"])?.into_iter().collect();
    Some(all.into_iter().filter(|p| !ignored.contains(p)).collect())
}

fn split_z(bytes: &[u8]) -> Vec<PathBuf> {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(String::from_utf8_lossy(s).into_owned()))
        .filter(|p| p.components().all(|c| matches!(c, Component::Normal(_))))
        .collect()
}

/// The files of a folder outside git, relative to it, up to [`FILES`], and
/// how many more there are. Hidden files are left out, and `.gitignore`
/// files are still followed.
fn walk_files(root: &Path, stop: &AtomicBool) -> (Vec<PathBuf>, usize) {
    let mut files = Vec::new();
    let mut past = 0;
    let walk = ignore::WalkBuilder::new(root)
        .require_git(false)
        .follow_links(false)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|e| e.path().strip_prefix(root).ok().map(Path::to_path_buf))
        .filter(|p| !is_secret_path(p));
    for rel in walk {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        if files.len() < FILES {
            files.push(rel);
        } else {
            past += 1;
        }
    }
    (files, past)
}

// ---- the answer ------------------------------------------------------------------

/// Secrets recognized by their shape alone, replaced whole.
static SHAPES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // A PEM private key, to its end or the end of the text.
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----(?s:.*?)(?:-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----|\z)",
        // OpenAI, Anthropic and other `sk-` keys.
        r"\bsk-[A-Za-z0-9_-]{20,}",
        // GitHub tokens.
        r"\bgh[pousr]_[A-Za-z0-9]{30,}",
        r"\bgithub_pat_[A-Za-z0-9_]{22,}",
        // GitLab and npm tokens.
        r"\bglpat-[A-Za-z0-9_-]{20,}",
        r"\bnpm_[A-Za-z0-9]{36}\b",
        // Slack tokens.
        r"\bxox[abposr]-[A-Za-z0-9-]{10,}",
        // AWS access key IDs.
        r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
        // Google API keys.
        r"\bAIza[0-9A-Za-z_-]{35}",
        // Stripe keys.
        r"\b[rsp]k_(?:live|test)_[0-9A-Za-z]{16,}",
        // JSON Web Tokens.
        r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("secret pattern"))
    .collect()
});

/// `user:password@` in a URL: the password is replaced. The user can be
/// empty (`redis://:pass@host`), and the password runs to the last `@`
/// before the host, since a password can hold one.
static URL_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://[^\s:/@]*:)([^\s/?#]*)@").expect("url pattern")
});

/// The token after `Bearer`, as in an `Authorization` header.
static BEARER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b([Bb]earer[ \t]+)([A-Za-z0-9._~+/=-]+)").expect("bearer pattern")
});

/// `name = value`, `name: value` and `"name": "value"`. Group 1 is the name,
/// 2 what stands between, and the value is double, single or back quoted
/// (3, 4, 5) or bare (6). [`names_secret`] decides on the name.
static ASSIGNED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\b([A-Za-z0-9_.-]+)(["']?[ \t]*(?::=|=>|:|=)[ \t]*)(?:"([^"\n]*)"|'([^'\n]*)'|`([^`\n]*)`|(\S+))"#,
    )
    .expect("assignment pattern")
});

/// A call or a path through fields: `get_token()`, `self.token`,
/// `os.environ["KEY"]`. Code, not a value.
static CODE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^[A-Za-z_][A-Za-z0-9_]*(?:(?:\.|::|->)[A-Za-z_][A-Za-z0-9_]*)*(?:!?\(|(?:(?:\.|::|->)[A-Za-z_][A-Za-z0-9_]*|\[.*)$)",
    )
    .expect("code pattern")
});

/// A type after `name:`, as in a struct field or a parameter.
static TYPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Z][A-Za-z0-9_]*(?:::[A-Za-z0-9_]+)*(?:<.*)?$").expect("type pattern")
});

/// Words that, as the last word of a name before `key`, say the key is not
/// a secret.
const PLAIN_KEYS: &[&str] = &[
    "public",
    "primary",
    "foreign",
    "sort",
    "cache",
    "partition",
    "map",
    "lookup",
    "row",
    "group",
    "idempotency",
    "hot",
    "short",
    "shortcut",
    "unique",
    "composite",
    "dedup",
    "routing",
    "react",
    "list",
    "dict",
    "hash",
    "object",
    "storage",
    "registry",
    "translation",
    "i18n",
    "query",
    "index",
];

/// Values that are code or a placeholder, never a secret.
const PLAIN_VALUES: &[&str] = &[
    "null",
    "none",
    "nil",
    "undefined",
    "true",
    "false",
    "await",
    "new",
    "yield",
    "async",
    "function",
    "lambda",
    "this",
    "self",
    "typeof",
    "str",
    "string",
    "number",
    "bool",
    "boolean",
    "bytes",
    "int",
    "float",
    "any",
    "usize",
    "u8",
    "u32",
    "u64",
    "i32",
    "i64",
];

/// Replaces keys, tokens, passwords and credentials in `text` with
/// [`REDACTED`]. Code and prose around them stay as they are.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for re in SHAPES.iter() {
        if re.is_match(&out) {
            out = re.replace_all(&out, REDACTED).into_owned();
        }
    }
    out = URL_PASSWORD
        .replace_all(&out, |c: &Captures| {
            if c[2].is_empty() || is_placeholder(&c[2]) {
                c[0].to_string()
            } else {
                format!("{}{REDACTED}@", &c[1])
            }
        })
        .into_owned();
    out = BEARER
        .replace_all(&out, |c: &Captures| {
            let v = &c[2];
            // "Bearer authentication" is prose; a token has digits or is
            // long and random-looking.
            if v.chars().any(|c| c.is_ascii_digit()) || (v.len() >= 20 && entropy(v) >= 3.5) {
                format!("{}{REDACTED}", &c[1])
            } else {
                c[0].to_string()
            }
        })
        .into_owned();
    redact_assigned(&out)
}

/// Replaces the value of every assignment whose name says secret, whatever
/// the value looks like, unless it is code or a placeholder.
fn redact_assigned(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut done = 0;
    let mut at = 0;
    while let Some(c) = ASSIGNED.captures_at(text, at) {
        let (whole, name) = (c.get(0).expect("match"), c.get(1).expect("name"));
        if !names_secret(name.as_str()) {
            // The value may hold the next name, as in `a=password=x`.
            at = name.end();
            continue;
        }
        let value = (3..=6).find_map(|i| c.get(i)).expect("value");
        let bare = c.get(6).is_some();
        // A bare value ends at a space; what closes it is kept.
        let end = if bare {
            let v = value.as_str();
            value.start()
                + v.trim_end_matches([',', ';', ')', ']', '}', '>', '.', '"', '\'', '`'])
                    .len()
        } else {
            value.end()
        };
        let v = &text[value.start()..end];
        let sep = c[2].trim().trim_start_matches(['"', '\'']);
        let keep = v.is_empty()
            || is_placeholder(v)
            || (bare && is_code(v, name.as_str(), sep, &text[end..]));
        if !keep {
            // A quoted value loses its quotes with it.
            let from = if bare {
                value.start()
            } else {
                value.start() - 1
            };
            let to = if bare { end } else { whole.end() };
            out.push_str(&text[done..from]);
            out.push_str(REDACTED);
            done = to;
        }
        at = whole.end().max(name.end());
    }
    out.push_str(&text[done..]);
    out
}

/// Whether `name` says its value is a secret: its last word is a password,
/// secret, token or credential, or a key other than a plain one
/// (`ENCRYPTION_KEY`, `apiKey`, but not `primary_key`). `PWD` is the shell's
/// working folder.
fn names_secret(name: &str) -> bool {
    let words = words(name);
    let Some(last) = words.last() else {
        return false;
    };
    const ENDS: &[&str] = &[
        "password",
        "passwd",
        "passphrase",
        "secret",
        "token",
        "credential",
        "credentials",
        "apikey",
        "secretkey",
        "privatekey",
        "accesskey",
    ];
    if ENDS.iter().any(|e| last.ends_with(e)) {
        return true;
    }
    match last.as_str() {
        "pass" => true,
        "pwd" => words.len() > 1,
        "key" => words.len() > 1 && !PLAIN_KEYS.contains(&words[words.len() - 2].as_str()),
        _ => false,
    }
}

/// The words of a name, in lower case: `DB_PASSWORD`, `db.password` and
/// `dbPassword` all end in `password`.
fn words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    for part in name.split(|c: char| !c.is_ascii_alphanumeric()) {
        let chars: Vec<char> = part.chars().collect();
        let mut word = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let prev = i.checked_sub(1).map(|j| chars[j]);
            let next = chars.get(i + 1);
            let starts = c.is_ascii_uppercase()
                && prev.is_some_and(|p| {
                    p.is_ascii_lowercase()
                        || p.is_ascii_digit()
                        || (p.is_ascii_uppercase() && next.is_some_and(char::is_ascii_lowercase))
                });
            if starts && !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            word.push(c.to_ascii_lowercase());
        }
        if !word.is_empty() {
            words.push(word);
        }
    }
    words
}

/// A value that only names where the secret comes from.
fn is_placeholder(v: &str) -> bool {
    v.contains(REDACTED)
        || v.starts_with("${")
        || v.starts_with("{{")
        || v.starts_with("%(")
        || v.starts_with("$(")
        || (v.starts_with('<') && v.ends_with('>'))
        || v.chars().all(|c| c == '*')
        // `$DB_PASSWORD`, but not a password like `$uperS3cret`.
        || v.strip_prefix('$').is_some_and(|n| {
            !n.is_empty() && n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        })
}

/// Whether a bare value is code rather than a secret: a call, a path
/// through fields, a reference, a keyword, the same or another secret's
/// name (`password=password`, `token = access_token`), or a type after
/// `name:` (`private_key: Ed25519KeyPair,`).
fn is_code(v: &str, name: &str, sep: &str, after: &str) -> bool {
    let ident = v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && v.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
    if v.starts_with('&')
        || CODE.is_match(v)
        || PLAIN_VALUES.contains(&v.to_ascii_lowercase().as_str())
        || (ident && (v.eq_ignore_ascii_case(name) || names_secret(v)))
    {
        return true;
    }
    // A type is followed by what ends a field or a parameter, or by the
    // default value.
    let closed = after.starts_with([',', ';', ')', '>', '{'])
        || after
            .trim_start_matches([' ', '\t'])
            .starts_with(['=', '{']);
    sep == ":" && closed && TYPE.is_match(v)
}

/// Shannon entropy of `s`, in bits per character.
fn entropy(s: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
    }
    let n = s.chars().count() as f64;
    counts
        .values()
        .map(|&k| {
            let p = k as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Redacts every text in the answer, and takes the copy's folder out of
/// paths so sources read as paths in the project.
pub fn clean_answer(a: &mut Answer, copy: Option<&Path>) {
    let prefix = copy.map(|p| {
        let mut s = p.to_string_lossy().into_owned();
        s.push(std::path::MAIN_SEPARATOR);
        s
    });
    let clean = |s: &mut String| {
        if let Some(prefix) = &prefix {
            *s = s.replace(prefix.as_str(), "");
        }
        *s = redact(s);
    };
    for idea in &mut a.ideas {
        clean(&mut idea.label);
        clean(&mut idea.title);
        clean(&mut idea.reason);
    }
    for v in &mut a.verified {
        clean(&mut v.text);
        clean(&mut v.source);
    }
    a.assumed.iter_mut().for_each(clean);
    a.unchecked.iter_mut().for_each(clean);
    clean(&mut a.decision);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_looking_paths_are_left_out() {
        for p in [
            ".env",
            ".env.local",
            "app/.env.production",
            ".envrc",
            ".env/config",
            "certs/server.pem",
            "tls/Server.KEY",
            "keys/store.p12",
            "id_rsa",
            "deploy/id_ed25519.pub",
            "keys/id_ecdsa",
            "id_rsa_github",
            "id_rsa.bak",
            "id_ed25519.old",
            "id_ed25519_sk",
            ".env-production",
            ".env_local",
            ".envrc.local",
            ".envs/.production/.django",
            ".ssh/config",
            "home/.ssh/known_hosts",
            ".aws/credentials",
            ".netrc",
            "web/.npmrc",
        ] {
            assert!(is_secret_path(Path::new(p)), "{p} should be left out");
        }
        for p in [
            "src/main.rs",
            "README.md",
            "docs/environment.md",
            "src/keyboard.rs",
            "src/key.rs.bak",
            "monkey.txt",
            "src/env.rs",
            "lib/identity.ts",
            "ssh/notes.md",
            "src/id_token.rs",
            "id_generator.py",
            "id_map.go",
            ".environment/setup.md",
            "src/.envoy.yaml",
        ] {
            assert!(!is_secret_path(Path::new(p)), "{p} should stay");
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = crate::git::git(dir)
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn the_copy_leaves_out_secrets_ignored_files_and_links() {
        let base = std::env::temp_dir().join(format!(
            "arakun-bot-guard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let (project, copy) = (base.join("project"), base.join("copy"));
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::create_dir_all(project.join(".ssh")).unwrap();
        std::fs::create_dir_all(&copy).unwrap();
        let write = |p: &str, s: &str| std::fs::write(project.join(p), s).unwrap();
        write("src/main.rs", "fn main() {}");
        write("notes.md", "untracked but not ignored");
        write(".gitignore", "secrets.txt\nbuild/\n");
        write("secrets.txt", "ignored");
        write("tracked-ignored.txt", "tracked, then ignored");
        write(".env", "API_KEY=1");
        write("server.pem", "key");
        write(".ssh/id_rsa", "key");
        std::fs::create_dir_all(project.join("build")).unwrap();
        write("build/out.txt", "ignored dir");
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/hostname", project.join("link")).unwrap();
        write("settings.py", "SECRET_KEY = 'django-insecure-abc123'\n");
        std::fs::create_dir_all(project.join("vendor/lib")).unwrap();
        std::fs::write(project.join("vendor/lib/lib.rs"), "").unwrap();
        git(&project.join("vendor/lib"), &["init", "-q"]);
        git(&project.join("vendor/lib"), &["add", "lib.rs"]);
        git(
            &project.join("vendor/lib"),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "x",
            ],
        );
        git(&project, &["init", "-q"]);
        git(
            &project,
            &[
                "add",
                "-f",
                "src/main.rs",
                "tracked-ignored.txt",
                ".env",
                "vendor/lib",
            ],
        );
        std::fs::write(
            project.join(".gitignore"),
            "secrets.txt\nbuild/\ntracked-ignored.txt\n",
        )
        .unwrap();

        let m = mirror(&project, &copy, &AtomicBool::new(false)).unwrap();
        let mut got: Vec<String> = ignore::WalkBuilder::new(&copy)
            .standard_filters(false)
            .build()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
            .map(|e| {
                e.path()
                    .strip_prefix(&copy)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        got.sort();
        assert_eq!(
            got,
            [".gitignore", "notes.md", "settings.py", "src/main.rs"]
        );
        assert_eq!(m.files, 4);
        assert_eq!(m.nested, [PathBuf::from("vendor/lib")]);
        assert_eq!(
            std::fs::read_to_string(copy.join("settings.py")).unwrap(),
            "SECRET_KEY = [redacted]\n"
        );

        let stopped = base.join("stopped");
        std::fs::create_dir_all(&stopped).unwrap();
        assert_eq!(
            mirror(&project, &stopped, &AtomicBool::new(true)),
            Err("stopped".into())
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_answer_names_what_the_copy_left_out() {
        let m = Mirror {
            files: 10,
            too_big: 1,
            failed: 3,
            nested: vec![PathBuf::from("vendor/lib")],
        };
        assert_eq!(
            m.notes(),
            [
                "1 file of the project was too large to read.",
                "3 files of the project could not be copied to read.",
                "Submodules and nested repositories were not read: vendor/lib.",
            ]
        );
        assert!(Mirror::default().notes().is_empty());
    }

    #[test]
    fn redacts_keys_tokens_and_passwords() {
        let cases = [
            (
                "key: sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123",
                "key: [redacted]",
            ),
            (
                "use sk-proj-ABCDEFGHIJKLMNOPQRSTUV here",
                "use [redacted] here",
            ),
            ("ghp_abcdefghijklmnopqrstuvwxyz0123456789", "[redacted]"),
            (
                "github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz",
                "[redacted]",
            ),
            ("slack xoxb-1234567890-abcdefghij", "slack [redacted]"),
            ("AKIAIOSFODNN7EXAMPLE is the id", "[redacted] is the id"),
            (
                "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
                "Bearer [redacted]",
            ),
            ("password=hunter22", "password=[redacted]"),
            ("DB_PASSWORD: \"correct horse\"", "DB_PASSWORD: [redacted]"),
            ("DB_PASSWORD: \"c0rrect-h0rse\"", "DB_PASSWORD: [redacted]"),
            (r#""api_key": "abcd1234efgh""#, r#""api_key": [redacted]"#),
            (
                "export GITHUB_TOKEN=abc123def456",
                "export GITHUB_TOKEN=[redacted]",
            ),
            (
                "postgres://app:s3cr3t@db.internal:5432/app",
                "postgres://app:[redacted]@db.internal:5432/app",
            ),
            (
                "client_secret = Zm9vYmFyYmF6cXV4cXV1eHh5eg",
                "client_secret = [redacted]",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(redact(input), want, "{input}");
        }
    }

    #[test]
    fn redacts_any_value_after_a_secret_name() {
        let cases = [
            (
                "POSTGRES_PASSWORD: example",
                "POSTGRES_PASSWORD: [redacted]",
            ),
            ("password=Summer@Time!", "password=[redacted]"),
            (
                "DB_PASSWORD=correct-horse-battery-staple",
                "DB_PASSWORD=[redacted]",
            ),
            ("secret=ABCDEFGHIJKLMNOPQRSTUVWX", "secret=[redacted]"),
            ("password=abc123$xyz789", "password=[redacted]"),
            ("password=$uperS3cret99", "password=[redacted]"),
            ("DB_PASSWORD=P@ss,word1x", "DB_PASSWORD=[redacted]"),
            ("password: \"it's-a-secret1\"", "password: [redacted]"),
            ("API_TOKEN='ab1'", "API_TOKEN=[redacted]"),
            ("a=password=hunter22", "a=password=[redacted]"),
            (
                "The .env sets API_TOKEN=ab12cd34.",
                "The .env sets API_TOKEN=[redacted].",
            ),
            ("dbPassword = hunter22", "dbPassword = [redacted]"),
            (
                "spring.datasource.password=foo",
                "spring.datasource.password=[redacted]",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(redact(input), want, "{input}");
        }
        let long = format!("client_secret: \"{}\"", "x".repeat(300));
        assert_eq!(redact(&long), "client_secret: [redacted]");
    }

    #[test]
    fn redacts_names_that_slipped_through() {
        let cases = [
            ("DB_PASS=hunter22", "DB_PASS=[redacted]"),
            ("SMTP_PASS=mailpass", "SMTP_PASS=[redacted]"),
            (
                "ENCRYPTION_KEY=9f86d081884c7d659a2feaa0c55ad015",
                "ENCRYPTION_KEY=[redacted]",
            ),
            ("apiKey: abcdef", "apiKey: [redacted]"),
            (
                "Authorization: Bearer abc123opaque456tokenvalue",
                "Authorization: Bearer [redacted]",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(redact(input), want, "{input}");
        }
    }

    #[test]
    fn redacts_a_password_with_no_user_or_with_an_at() {
        let cases = [
            (
                "REDIS_URL=redis://:s3cretpass@cache:6379/0",
                "REDIS_URL=redis://:[redacted]@cache:6379/0",
            ),
            (
                "postgres://app:p@ss@host/db",
                "postgres://app:[redacted]@host/db",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(redact(input), want, "{input}");
        }
    }

    #[test]
    fn redacts_a_whole_private_key() {
        let text = "Here it is:\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nAAAABG5vbmUAAAAEbm9uZQ==\n-----END OPENSSH PRIVATE KEY-----\nDone.";
        assert_eq!(redact(text), "Here it is:\n[redacted]\nDone.");
        let cut = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow";
        assert_eq!(redact(cut), "[redacted]");
    }

    #[test]
    fn leaves_code_and_prose_alone() {
        for text in [
            "let token = self.next_token();",
            "password: String,",
            "fn read_password(prompt: &str) -> String",
            "token = get_token()",
            "The API key goes in the settings, never in the repo.",
            "Set `api_key` to your own key: api_key = ${API_KEY}",
            "url = \"https://example.com/path?q=1\"",
            "secret = config.secret_key",
            "auth = AuthConfig::new()",
            "TOKEN_BYTES = 32",
            "git@github.com:ArakunHQ/arakun.git",
            "https://user@example.com/repo",
            "Option B: keep the session id in the token store [3] [4]",
            "postgres://app:${DB_PASSWORD}@db/app",
            "password: \"<your password>\"",
            "Use skeleton-loading for the list",
            "sha256: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "private_key: Ed25519KeyPair,",
            "token = base64url(bytes)",
            "token_type: \"Bearer\"",
            "tokenizer = \"cl100k_base\"",
            "PWD=/home/ann/project1",
            "src/password.rs:L120-L140",
            "password: Option<String>,",
            "fn connect(password: &str, token: Token) -> Result<()>",
            "api_key = os.environ[\"API_KEY\"]",
            "password = password",
            "token = access_token",
            "password = None",
            "const token = await fetchToken();",
            "export PGPASSWORD=$DB_PASSWORD",
            "primary_key = id",
            "Use Bearer authentication for the API.",
            "https://user@example.com/repo",
        ] {
            assert_eq!(redact(text), text, "{text}");
        }
    }

    #[test]
    fn cleans_every_text_in_the_answer() {
        let mut a = super::super::bot::parse_answer(
            r#"{"ideas":[{"label":"A","title":"Keep it","verdict":"works","reason":"the key is sk-ant-0123456789abcdefghijkl","cites":[1]}],
               "verified":[{"text":"password=hunter22 is set","source":"/tmp/arakun-bot-x/config/app.toml"}],
               "assumed":[],"unchecked":[],"decision":"Rotate ghp_abcdefghijklmnopqrstuvwxyz0123456789."}"#,
            1,
        )
        .unwrap();
        clean_answer(&mut a, Some(Path::new("/tmp/arakun-bot-x")));
        assert_eq!(a.ideas[0].reason, "the key is [redacted]");
        assert_eq!(a.verified[0].text, "password=[redacted] is set");
        assert_eq!(a.verified[0].source, "config/app.toml");
        assert_eq!(a.decision, "Rotate [redacted].");
    }
}
