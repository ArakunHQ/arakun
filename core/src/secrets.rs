//! Finds secrets: keys, tokens and passwords in text, and the files that hold
//! them. Redacted mode, telemetry and crash reports, @bot's guard, Claude
//! Code's messages and the sandbox all ask here, so what one of them catches,
//! all of them catch.
//!
//! [`find`] says where the secrets are, as byte ranges, and each caller hides
//! them its own way: dots of the same width, `[redacted]`, `<secret>`.

use std::ops::Range;
use std::path::{Component, Path};
use std::sync::LazyLock;

use regex::Regex;

/// What [`redact`] puts in place of a secret, as @bot's guard uses it. A
/// value that already says it is a placeholder.
pub const REDACTED: &str = "[redacted]";

/// How a secret was recognized, which says how much of it is safe to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// By its shape: a known prefix such as `ghp_` or `sk-`, or a long word
    /// that reads as random. Its first few characters say what it is and
    /// give nothing away.
    Token,
    /// A private key's PEM block, header to footer.
    PrivateKey,
    /// By where it stands: after a secret's name (`DB_PASSWORD=`), after
    /// `Bearer` or `Basic`, or as the password in a URL. Nothing of it is
    /// safe to show.
    Value,
}

/// A secret in a text: where it is, in bytes, and how it was recognized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Secret {
    pub range: Range<usize>,
    pub kind: Kind,
}

/// The secrets in `text`, in order and without overlaps. Where two rules
/// find the same secret, the one that starts first wins, and a [`Kind::Token`]
/// over the others, so a known key keeps its recognizable start.
pub fn find(text: &str) -> Vec<Secret> {
    let mut found = Vec::new();
    for m in SHAPES.find_iter(text) {
        let kind = if m.as_str().starts_with("-----BEGIN") {
            Kind::PrivateKey
        } else {
            Kind::Token
        };
        found.push(Secret {
            range: m.range(),
            kind,
        });
    }
    let value = |m: regex::Match<'_>| Secret {
        range: m.range(),
        kind: Kind::Value,
    };
    for c in URL_PASSWORD.captures_iter(text) {
        let pass = c.get(2).expect("password");
        if !pass.is_empty() && !is_placeholder(pass.as_str()) {
            found.push(value(pass));
        }
    }
    for c in BEARER.captures_iter(text) {
        let token = c.get(2).expect("token");
        // The end of a sentence is not part of the token.
        let end = token.start() + token.as_str().trim_end_matches('.').len();
        let v = &text[token.start()..end];
        if is_bearer_token(v) {
            found.push(Secret {
                range: token.start()..end,
                kind: Kind::Value,
            });
        }
    }
    for c in BASIC.captures_iter(text) {
        let creds = c.get(2).expect("credentials");
        if is_basic_credentials(creds.as_str()) {
            found.push(value(creds));
        }
    }
    assigned(text, &mut found);
    random_words(text, &mut found);

    found.sort_by_key(|s| (s.range.start, std::cmp::Reverse(s.range.end), s.kind));
    let mut out: Vec<Secret> = Vec::with_capacity(found.len());
    for s in found {
        match out.last_mut() {
            Some(last) if s.range.start < last.range.end => {
                last.range.end = last.range.end.max(s.range.end);
            }
            _ => out.push(s),
        }
    }
    out
}

/// `text` with every secret in it replaced with `with`. Quotes around a
/// value stay, so code and JSON keep their shape.
pub fn redact(text: &str, with: &str) -> String {
    let found = find(text);
    if found.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut done = 0;
    for s in found {
        out.push_str(&text[done..s.range.start]);
        out.push_str(with);
        done = s.range.end;
    }
    out.push_str(&text[done..]);
    out
}

// ---- shapes ------------------------------------------------------------------

/// Secrets recognized by their shape alone, as one pattern so a text is read
/// once.
static SHAPES: LazyLock<Regex> = LazyLock::new(|| {
    let shapes = [
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
        // Slack tokens, and a webhook's URL, which posts for whoever has it.
        r"\b(?:xox[abposr]|xapp)-[A-Za-z0-9-]{10,}",
        r"\bhttps://hooks\.slack\.com/(?:services|workflows|triggers)/[A-Za-z0-9_/-]+",
        // AWS access key IDs.
        r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
        // Google API keys and OAuth access tokens.
        r"\bAIza[0-9A-Za-z_-]{35}",
        r"\bya29\.[A-Za-z0-9_-]{20,}",
        // Stripe keys and webhook signing secrets.
        r"\b[rsp]k_(?:live|test)_[0-9A-Za-z]{16,}",
        r"\bwhsec_[A-Za-z0-9+/=]{24,}",
        // PostHog project, personal and secret keys.
        r"\bph[cxs]_[A-Za-z0-9]{20,}",
        // Supabase access tokens.
        r"\bsbp_[A-Za-z0-9]{40}\b",
        // Hugging Face, SendGrid, PyPI, DigitalOcean and Shopify tokens.
        r"\bhf_[A-Za-z0-9]{30,}",
        r"\bSG\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}",
        r"\bpypi-[A-Za-z0-9_-]{50,}",
        r"\bdo[opr]_v1_[a-f0-9]{64}\b",
        r"\bshp(?:at|ca|pa|ss)_[a-fA-F0-9]{32}\b",
        // JSON Web Tokens.
        r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
    ];
    let one: Vec<String> = shapes.iter().map(|s| format!("(?:{s})")).collect();
    Regex::new(&one.join("|")).expect("secret shapes")
});

/// Long words that read as random: at least 24 characters of letters in both
/// cases, digits and the few marks base64 and tokens use, spread out enough
/// to be noise and not a name. A word with `.`, `@` or `%` in it is a host, an
/// address or an encoded URL, and one that starts with `/` is a path.
fn random_words(text: &str, out: &mut Vec<Secret>) {
    let b = text.as_bytes();
    let run = |c: u8| c.is_ascii_alphanumeric() || b"_-+/=.@%~".contains(&c);
    let mut i = 0;
    while i < b.len() {
        if !run(b[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && run(b[i]) {
            i += 1;
        }
        let word = &text[start..i];
        if word.contains(['.', '@', '%', '~']) || word.starts_with('/') {
            continue;
        }
        // `id=AbC…` is a name and its value: only padding ends a token with `=`.
        let mut from = start;
        for part in split_assignments(word) {
            if looks_random(part) {
                out.push(Secret {
                    range: from..from + part.len(),
                    kind: Kind::Token,
                });
            }
            from += part.len() + 1;
        }
    }
}

/// `word` split at each `=` that is not base64's padding at its end.
fn split_assignments(word: &str) -> impl Iterator<Item = &str> {
    let body = word.trim_end_matches('=');
    let pad = word.len() - body.len();
    let parts: Vec<&str> = body.split('=').collect();
    let last = parts.len() - 1;
    parts.into_iter().enumerate().map(move |(i, p)| {
        if i == last {
            // Back to `word`, so the padding is part of the token.
            let start = p.as_ptr() as usize - word.as_ptr() as usize;
            &word[start..start + p.len() + pad]
        } else {
            p
        }
    })
}

fn looks_random(word: &str) -> bool {
    let core = word.trim_end_matches('=');
    if core.len() < 24 || core.ends_with('/') {
        return false;
    }
    let letters = core.chars().filter(char::is_ascii_alphabetic).count();
    let digits = core.chars().filter(char::is_ascii_digit).count();
    let upper = core.chars().any(|c| c.is_ascii_uppercase());
    let lower = core.chars().any(|c| c.is_ascii_lowercase());
    digits >= 3 && letters >= 8 && upper && lower && entropy(core) >= 3.5
}

// ---- values by where they stand ------------------------------------------------

/// `user:password@` in a URL: the password is the secret. The user can be
/// empty (`redis://:pass@host`), and the password runs to the last `@`
/// before the host, since a password can hold one.
static URL_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://[^\s:/@]*:)([^\s/?#]*)@").expect("url pattern")
});

/// The token after `Bearer`, as in an `Authorization` header.
static BEARER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b([Bb]earer[ \t]+)([A-Za-z0-9._~+/=-]+)").expect("bearer pattern")
});

/// What follows `Basic` in an `Authorization` header: `user:password` in
/// base64, which anyone can decode.
static BASIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b([Bb]asic[ \t]+)([A-Za-z0-9+/]{12,}={0,2})").expect("basic pattern")
});

/// Whether what follows `Bearer` is a token and not a word: "Bearer
/// authentication" is prose. A token has a digit or a mark in it, or is long
/// and random-looking.
fn is_bearer_token(v: &str) -> bool {
    v.chars()
        .any(|c| c.is_ascii_digit() || "._~+/=".contains(c))
        || (v.len() >= 20 && entropy(v) >= 3.5)
}

/// Whether what follows `Basic` is base64 and not a word: "Basic
/// authentication" is prose. Base64 of `user:password` has a digit, padding
/// or both cases in it.
fn is_basic_credentials(v: &str) -> bool {
    v.ends_with('=')
        || v.chars().any(|c| c.is_ascii_digit())
        || (v.chars().any(|c| c.is_ascii_uppercase())
            && v.chars().skip(1).any(|c| c.is_ascii_uppercase())
            && v.chars().any(|c| c.is_ascii_lowercase())
            && entropy(v) >= 3.5)
}

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

/// The value of every assignment whose name says secret, whatever the value
/// looks like, unless it is code or a placeholder. A quoted value is the part
/// inside the quotes.
fn assigned(text: &str, out: &mut Vec<Secret>) {
    let mut at = 0;
    while let Some(c) = ASSIGNED.captures_at(text, at) {
        let (whole, name, sep) = (
            c.get(0).expect("match"),
            c.get(1).expect("name"),
            c.get(2).expect("separator"),
        );
        // `==` and `===` compare, `::` walks a path: `event.key == "Enter"`
        // and `crypto_key::generate()` assign nothing.
        let op_end = sep.start() + sep.as_str().trim_end_matches([' ', '\t']).len();
        let op = text[..op_end].chars().next_back();
        let next = text[op_end..].chars().next();
        let compares = matches!((op, next), (Some('='), Some('=')) | (Some(':'), Some(':')));
        if compares || !names_secret(name.as_str()) {
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
            out.push(Secret {
                range: value.start()..end,
                kind: Kind::Value,
            });
        }
        at = whole.end().max(name.end());
    }
}

/// Whether `name` says its value is a secret: its last word is a password,
/// secret, token or credential, or a key other than a plain one
/// (`ENCRYPTION_KEY`, `apiKey`, but not `primary_key`). `PWD` is the shell's
/// working folder, and `KEYBOARD` or `SSH_KEY_PATH` only name where one is.
pub fn names_secret(name: &str) -> bool {
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

/// Whether a field called `name` may hold a secret, when there is no value
/// to look at: any word of it about keys, tokens, passwords or signing in.
/// Looser than [`names_secret`], for asking before a value is written where
/// others read it.
pub fn may_name_secret(name: &str) -> bool {
    names_secret(name)
        || words(name).iter().any(|w| {
            [
                "key",
                "token",
                "secret",
                "password",
                "passwd",
                "auth",
                "credential",
            ]
            .iter()
            .any(|s| w.contains(s))
        })
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

/// A value that only names where the secret comes from, or was hidden
/// already.
fn is_placeholder(v: &str) -> bool {
    v.contains(REDACTED)
        || v.starts_with("${")
        || v.starts_with("{{")
        || v.starts_with("%(")
        || v.starts_with("$(")
        || (v.starts_with('<') && v.ends_with('>'))
        || v.chars().all(|c| c == '*' || c == '•')
        // `$DB_PASSWORD`, but not a password like `$uperS3cret`.
        || v.strip_prefix('$').is_some_and(|n| {
            !n.is_empty() && n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        })
}

/// Whether a bare value is code rather than a secret: a call, a path
/// through fields, a reference, a keyword, a small number (`max_token =
/// 100`), the same or another secret's name (`password=password`, `token =
/// access_token`), or a type after `name:` (`private_key: Ed25519KeyPair,`).
fn is_code(v: &str, name: &str, sep: &str, after: &str) -> bool {
    let ident = v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && v.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
    if v.starts_with('&')
        || CODE.is_match(v)
        || PLAIN_VALUES.contains(&v.to_ascii_lowercase().as_str())
        || (v.len() < 6 && v.chars().all(|c| c.is_ascii_digit()))
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

// ---- files ---------------------------------------------------------------------

/// Folders whose files are all secrets.
const SECRET_DIRS: &[&str] = &[".ssh", ".gnupg", ".aws", ".kube", ".azure"];
/// Folders of secrets that only count under their parent: `gcloud` alone is
/// a common name for code.
const SECRET_SUBDIRS: &[[&str; 2]] = &[[".config", "gcloud"]];
/// File names that hold credentials.
const SECRET_NAMES: &[&str] = &[
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    ".pgpass",
    ".htpasswd",
    ".modal.toml",
    "kubeconfig",
    "auth.json",
    "credentials",
    "credentials.json",
    "credentials.toml",
];
/// Files that hold credentials only in their own folder.
const SECRET_FILES: &[[&str; 2]] = &[[".docker", "config.json"]];
/// How the JSON key files of cloud accounts start: `service-account.json`,
/// `service_account-prod.json`, `client_secret_123.json`.
const SECRET_JSON: &[&str] = &[
    "service-account",
    "service_account",
    "serviceaccount",
    "client_secret",
];
/// What a name ends with when Terraform wrote state or variables there:
/// state holds every secret of what it manages, in the clear.
const SECRET_ENDINGS: &[&str] = &[".tfstate.backup", ".tfvars.json"];
/// SSH key files, as `ssh-keygen` names them. A name that goes on from one
/// with `_` or `.` (`id_rsa_github`, `id_ed25519.old`) is a key as well.
const SSH_KEYS: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];
/// Endings of key, certificate and password store files.
const SECRET_EXTENSIONS: &[&str] = &[
    "pem", "key", "p12", "pfx", "jks", "keystore", "ppk", "kdbx", "gpg", "asc", "tfstate", "tfvars",
];
/// Endings of data files, where a word in the name says what is in it.
const DATA_EXTENSIONS: &[&str] = &[
    "", "json", "txt", "toml", "yaml", "yml", "ini", "conf", "cfg",
];
/// Words that, in a data file's name, say it holds a secret.
const SECRET_WORDS: &[&str] = &[
    "secret", "password", "passwd", "wallet", "keypair", "apikey", "api_key", "api-key",
];

/// Whether a file name is one tools keep environment variables in: `.env`,
/// `.env.*`, `.env-*`, `.env_*`, `.envrc*`, `*.env`, `.envs` and
/// Wrangler's `.dev.vars`. `.environment` and `.envoy.yaml` are not.
pub fn env_file(name: &str) -> bool {
    let n = name.to_lowercase();
    n == ".env"
        || n == ".envs"
        || n.starts_with(".envrc")
        || [".env.", ".env-", ".env_"].iter().any(|p| n.starts_with(p))
        || n.ends_with(".env")
        || n == ".dev.vars"
        || n.starts_with(".dev.vars.")
}

/// Whether a file, by its name alone, looks like it holds secrets: an
/// [`env_file`], a key or key store (`*.pem`, `*.key`, `*.p12`, `id_rsa`),
/// a credentials file (`.netrc`, `.pgpass`, `kubeconfig`, a service
/// account's JSON key), Terraform state and variables, or a data file named
/// for a secret (`password.json`, `gh-token.txt`, but not `tokenizer.json`
/// or `password_reset.rs`). Case doesn't matter.
pub fn secret_file(name: &str) -> bool {
    let n = name.to_lowercase();
    let (stem, ext) = n
        .rsplit_once('.')
        .filter(|(a, _)| !a.is_empty())
        .unwrap_or((n.as_str(), ""));
    // Words count in data files only, so password_reset.rs is code.
    let data = DATA_EXTENSIONS.contains(&ext);
    let ssh = SSH_KEYS.iter().any(|k| {
        n.strip_prefix(k)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['_', '.']))
    });
    env_file(&n)
        || ssh
        || SECRET_NAMES.contains(&n.as_str())
        || SECRET_ENDINGS.iter().any(|e| n.ends_with(e))
        || (ext == "json" && SECRET_JSON.iter().any(|p| n.starts_with(p)))
        || SECRET_EXTENSIONS.contains(&ext)
        || (data && SECRET_WORDS.iter().any(|w| stem.contains(w)))
        // token.json, gh-token.txt; not tokenizer.json.
        || (data && stem.split(['-', '_', '.']).any(|w| w == "token" || w == "tokens"))
}

/// Whether a path in a project looks like it holds secrets: a
/// [`secret_file`], anything under `.ssh/`, `.gnupg/`, `.aws/`, `.kube/`,
/// `.azure/`, `.config/gcloud/` or an env folder, and `.docker/config.json`.
pub fn secret_path(rel: &Path) -> bool {
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
    let env_dir = |d: &str| d == ".env" || d == ".envs";
    if dirs
        .iter()
        .any(|d| SECRET_DIRS.contains(&d.as_str()) || env_dir(d))
        || dirs
            .windows(2)
            .any(|w| SECRET_SUBDIRS.iter().any(|s| w == s))
    {
        return true;
    }
    let in_dir = |f: &[&str; 2]| dirs.last().is_some_and(|d| d == f[0]) && name == f[1];
    secret_file(name) || SECRET_FILES.iter().any(in_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(text: &str) -> String {
        redact(text, REDACTED)
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
            (
                "DB_PASSWORD: \"correct horse\"",
                "DB_PASSWORD: \"[redacted]\"",
            ),
            (
                "DB_PASSWORD: \"c0rrect-h0rse\"",
                "DB_PASSWORD: \"[redacted]\"",
            ),
            (r#""api_key": "abcd1234efgh""#, r#""api_key": "[redacted]""#),
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
            (
                "aws_secret_access_key wJalrXUtnFEMI/K7MDENG/bPxRfiCY9EXAMPLE2KEY",
                "aws_secret_access_key [redacted]",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(r(input), want, "{input}");
        }
    }

    /// Tokens of other services, by their shape. Each is put together here
    /// so no scanner takes this file for a leak.
    #[test]
    fn redacts_tokens_of_more_services() {
        let hex = |n: usize| "0a1b2c3d4e5f6789".repeat(n / 16 + 1)[..n].to_string();
        let mixed = |n: usize| "aB3dE5gH7jK9mN1p".repeat(n / 16 + 1)[..n].to_string();
        let tokens = [
            format!(
                "https://hooks.slack.com/services/T{}/B{}/{}",
                "0A1B2C3D",
                "4E5F6G7H",
                mixed(24)
            ),
            format!("xapp-1-A{}-{}-{}", "0123ABCD", "1234567890", hex(32)),
            format!("hf_{}", mixed(34)),
            format!("SG.{}.{}", mixed(22), mixed(43)),
            format!("whsec_{}", mixed(32)),
            format!("pypi-{}", mixed(70)),
            format!("dop_v1_{}", hex(64)),
            format!("ya29.{}", mixed(40)),
            format!("shpat_{}", hex(32)),
            format!("glpat-{}", mixed(20)),
            format!("npm_{}", mixed(36)),
            format!("AIza{}", mixed(35)),
            format!("ASIA{}", "0123456789ABCDEF"),
            format!("sk_{}_{}", "live", mixed(24)),
            format!("rk_{}_{}", "test", mixed(24)),
            format!("phc_{}", mixed(40)),
            format!("phx_{}", mixed(40)),
            format!("sbp_{}", hex(40)),
            format!("xoxp-{}-{}", "1234567890", mixed(24)),
        ];
        for token in tokens {
            assert_eq!(
                r(&format!("use {token} here")),
                "use [redacted] here",
                "{token}"
            );
            let found = find(&format!("use {token} here"));
            assert_eq!(found.len(), 1, "{token}");
            assert_eq!(found[0].kind, Kind::Token, "{token}");
        }
        let cases = [
            (
                "Authorization: Basic dXNlcjpwYXNzd29yZA==",
                "Authorization: Basic [redacted]",
            ),
            (
                "curl -H 'Authorization: Basic YWRtaW46aHVudGVyMjI'",
                "curl -H 'Authorization: Basic [redacted]'",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(r(input), want, "{input}");
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
            ("password: \"it's-a-secret1\"", "password: \"[redacted]\""),
            ("API_TOKEN='ab1'", "API_TOKEN='[redacted]'"),
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
            ("DB_PASS=hunter22", "DB_PASS=[redacted]"),
            ("SMTP_PASS=mailpass", "SMTP_PASS=[redacted]"),
            (
                "ENCRYPTION_KEY=9f86d081884c7d659a2feaa0c55ad015",
                "ENCRYPTION_KEY=[redacted]",
            ),
            ("apiKey: abcdef", "apiKey: [redacted]"),
            ("'password' => 'hunter22',", "'password' => '[redacted]',"),
            ("password=123456", "password=[redacted]"),
            (
                "Authorization: Bearer abc123opaque456tokenvalue",
                "Authorization: Bearer [redacted]",
            ),
            (
                "Authorization: Bearer abc.def",
                "Authorization: Bearer [redacted]",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(r(input), want, "{input}");
        }
        let long = format!("client_secret: \"{}\"", "x".repeat(300));
        assert_eq!(r(&long), "client_secret: \"[redacted]\"");
    }

    #[test]
    fn redacts_a_password_in_a_url() {
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
            assert_eq!(r(input), want, "{input}");
        }
    }

    #[test]
    fn redacts_a_whole_private_key() {
        let text = "Here it is:\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nAAAABG5vbmUAAAAEbm9uZQ==\n-----END OPENSSH PRIVATE KEY-----\nDone.";
        assert_eq!(r(text), "Here it is:\n[redacted]\nDone.");
        assert_eq!(find(text)[0].kind, Kind::PrivateKey);
        let cut = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow";
        assert_eq!(r(cut), "[redacted]");
    }

    #[test]
    fn finds_long_random_words() {
        let random = "Abc123Def456Ghi789Jkl012Mno";
        assert_eq!(r(&format!("x {random} x")), "x [redacted] x");
        // Only the value, not its name.
        assert_eq!(r(&format!("session_id={random}")), "session_id=[redacted]");
        // Base64 keeps its padding.
        assert_eq!(
            r("value: Zm9vYmFyQmF6UXV4MTIzNDU2Nzg5MA== ok"),
            "value: [redacted] ok"
        );
    }

    #[test]
    fn a_known_token_wins_over_where_it_stands() {
        let text = "https://me:ghp_FAKEtoken0000000000000000000000000000@github.com/a/b.git";
        let found = find(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, Kind::Token);
        assert_eq!(found[0].range, 11..text.find('@').unwrap());
    }

    /// Code, prose, hashes and ids that only look like secrets. ARAKUN-102:
    /// a comparison is not an assignment, and a small number is not a key.
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
            "git clone git@github.com:o/r.git",
            "https://user@example.com/repo",
            "Option B: keep the session id in the token store [3] [4]",
            "postgres://app:${DB_PASSWORD}@db/app",
            "password: \"<your password>\"",
            "Use skeleton-loading for the list",
            "sha256: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "commit 840ea5e1b0f3c2a9d8e7f6a5b4c3d2e1f0a9b8c7",
            "session 3f2a9c1e-4b5d-6f70-8192-a3b4c5d6e7f8",
            "private_key: Ed25519KeyPair,",
            "token = base64url(bytes)",
            "token_type: \"Bearer\"",
            "tokenizer = \"cl100k_base\"",
            "PWD=/home/ann/project1",
            "KEYBOARD=us SSH_KEY_PATH=/x PWD=/tmp",
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
            "Use Basic authentication for the API.",
            "Basic configuration of the server",
            "Basic TypeScript setup",
            "https://hooks.slack.com/ is where webhooks live",
            "the hf_hub_download helper",
            "pypi-publish runs on a tag",
            "SG.fn is the namespace",
            "npx @playwright/mcp@latest --viewport-size 1280x800",
            "https://mcp.supabase.com/mcp?project_ref=ahjnnuzxjuniajhxzrnw",
            "Bash(cargo build:*)",
            "listening on 127.0.0.1:3000",
            "initialize failed: connection refused (os error 111)",
            "/home/ann/Projects/Abc123Def456Ghi789Jkl012Mno",
            "com.example.Abc123Def456Ghi789Jkl012Mno",
            "if event.key == \"Enter\" {",
            "if (e.key === \"Escape\") return;",
            "if self.api_key == other {",
            "event.key != \"Tab\"",
            "max_token = 100",
            "csrf_token: 42,",
            "keyboard.key = 13",
            "const secret = 5 + offset;",
            "let key = crypto_key::generate();",
            "password => password.trim()",
        ] {
            assert_eq!(r(text), text, "{text}");
        }
    }

    #[test]
    fn names_of_secrets() {
        for n in [
            "OPENAI_API_KEY",
            "apiKey",
            "db.password",
            "DB_PASS",
            "client_secret",
            "GITHUB_TOKEN",
            "ENCRYPTION_KEY",
        ] {
            assert!(names_secret(n), "{n}");
        }
        for n in [
            "KEYBOARD",
            "PWD",
            "primary_key",
            "SSH_KEY_PATH",
            "key",
            "tokenizer",
        ] {
            assert!(!names_secret(n), "{n}");
        }
        for n in ["key", "AUTH_HEADER", "Authorization", "SECRET_ID"] {
            assert!(may_name_secret(n), "{n}");
        }
        assert!(!may_name_secret("PORT"));
    }

    #[test]
    fn secret_files_by_name_and_path() {
        for p in [
            ".env",
            ".env.local",
            "prod.env",
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
            "gcp/credentials.json",
            "service-account.json",
            "keys/Service_Account-prod.json",
            "client_secret_1234.apps.json",
            "infra/terraform.tfstate",
            "infra/terraform.tfstate.backup",
            "infra/prod.tfvars",
            "infra/prod.auto.tfvars.json",
            "kubeconfig",
            ".kube/config",
            "home/.docker/config.json",
            ".pgpass",
            "web/.htpasswd",
            ".dev.vars",
            ".dev.vars.staging",
            "putty/deploy.ppk",
            "vault.kdbx",
            ".azure/accessTokens.json",
            ".config/gcloud/application_default_credentials.json",
            "gh-token.txt",
            "token.json",
            "my-wallet.json",
            "password.json",
            "auth.json",
            "api_key",
            "backup.gpg",
        ] {
            assert!(secret_path(Path::new(p)), "{p} is a secret");
        }
        for p in [
            "src/main.rs",
            "README.md",
            "Cargo.toml",
            "docs/environment.md",
            "src/keyboard.rs",
            "keyboard.c",
            "src/key.rs.bak",
            "monkey.txt",
            "src/env.rs",
            "lib/identity.ts",
            "ssh/notes.md",
            "src/id_token.rs",
            "id_generator.py",
            "id_map.go",
            ".environment/setup.md",
            "docker/config.json",
            "src/config.json",
            "gcloud/deploy.sh",
            ".config/settings.json",
            "src/credentials.rs",
            "src/service_account.rs",
            "infra/main.tf",
            "infra/variables.tf",
            "src/dev.vars.md",
            "src/.envoy.yaml",
            "service.env.example",
            "token.rs",
            "tokenizer.json",
            "password_reset.rs",
        ] {
            assert!(!secret_path(Path::new(p)), "{p} is not a secret");
        }
        assert!(secret_file("PROD.ENV"));
        assert!(env_file(".env.example"));
        assert!(!env_file(".envoy.yaml"));
    }
}
