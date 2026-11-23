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

/// A name that says its value is a secret.
const KEY_NAME: &str = r"[A-Za-z0-9_.-]*(?:passw(?:or)?d|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|auth[_-]?key|credentials?|client[_-]?secret)[A-Za-z0-9_.-]*";

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

/// `user:password@` in a URL: the password is replaced.
static URL_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://[^\s:/@]+:)([^\s@/]+)@").expect("url pattern")
});

/// `name = value` and `"name": "value"` where the name says secret. The value
/// is quoted (group 2) or bare (group 3).
static ASSIGNED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?i)(\b{KEY_NAME}["']?\s*(?::=|=>|:|=)\s*)(?:["'`]([^"'`\n]{{4,200}})["'`]|([^\s"'`,;(){{}}\[\]<>$]{{6,}}))"#
    ))
    .expect("assignment pattern")
});

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
            if c[2].starts_with('$') || c[2].starts_with('{') || c[2] == *REDACTED {
                c[0].to_string()
            } else {
                format!("{}{REDACTED}@", &c[1])
            }
        })
        .into_owned();
    ASSIGNED
        .replace_all(&out, |c: &Captures| {
            let secret = match (c.get(2), c.get(3)) {
                (Some(quoted), _) => !is_placeholder(quoted.as_str()),
                (None, Some(bare)) => looks_secret(bare.as_str()),
                _ => false,
            };
            if secret {
                format!("{}{REDACTED}", &c[1])
            } else {
                c[0].to_string()
            }
        })
        .into_owned()
}

/// A quoted value that only names where the secret comes from.
fn is_placeholder(v: &str) -> bool {
    v == REDACTED || v.starts_with("${") || v.starts_with('<') || v.chars().all(|c| c == '*')
}

/// A bare value that is a secret rather than code: a mix of letters and
/// digits, or long and random-looking. `self.token`, `String` and
/// `read_password` stay.
fn looks_secret(v: &str) -> bool {
    if v == REDACTED || v.contains("::") {
        return false;
    }
    let letters = v.chars().any(|c| c.is_ascii_alphabetic());
    let digits = v.chars().any(|c| c.is_ascii_digit());
    let word = v
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-');
    // An identifier or a path through fields reads like code.
    let code = word
        && v.split(['.', '_', '-'])
            .all(|p| p.chars().all(|c| c.is_ascii_alphabetic()));
    if code {
        return false;
    }
    (letters && digits) || (v.len() >= 20 && entropy(v) >= 3.5)
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
