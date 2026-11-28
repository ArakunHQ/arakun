//! `widget.toml`: what a widget is, what it draws in and what it needs. A
//! manifest that breaks a rule is refused whole, before anything runs, so the
//! sandbox only ever meets needs that are well formed.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// The newest block version this Arakun draws.
pub const BLOCKS: u32 = 1;
/// The shortest refresh the spec allows.
pub const MIN_EVERY: u64 = 30;
/// The longest, so a typo can't park a widget for a year.
const MAX_EVERY: u64 = 24 * 60 * 60;
/// All of a widget's files together.
pub const MAX_FILES: usize = 512 * 1024;

/// The files a widget must have.
pub const REQUIRED: [&str; 3] = ["widget.toml", "main.rhai", "sample.json"];
/// The files it may also have. Nothing else is allowed.
pub const OPTIONAL: [&str; 3] = ["icon.svg", "README.md", "LICENSE"];

/// What the user approves, line by line, on the Needs step.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Needs {
    #[serde(default)]
    pub git: bool,
    #[serde(default)]
    pub sessions: bool,
    #[serde(default)]
    pub notices: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub net: Vec<Net>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file: Vec<FileNeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret: Vec<SecretNeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub action: Vec<ActionNeed>,
}

/// One host the widget may fetch from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Net {
    pub host: String,
    pub why: String,
    pub every: String,
}

/// One file in the project the widget may read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileNeed {
    pub path: String,
    pub why: String,
}

/// A token Arakun adds to requests to one host. The script only names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretNeed {
    pub name: String,
    pub label: String,
    pub host: String,
    pub header: String,
    /// The header's value, with `{}` where the token goes.
    pub format: String,
    /// Where to make a token, shown beside the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub get_one: Option<String>,
}

/// Something a button may do, after asking.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionNeed {
    pub kind: String,
    pub why: String,
}

/// The kinds of action there are.
pub const ACTIONS: [&str; 2] = ["start_session", "open_url"];

/// Where a widget can draw.
pub const SIZES: [&str; 4] = ["small", "wide", "tab", "bar"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Info {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub about: String,
    pub version: String,
    #[serde(default)]
    pub license: String,
    pub blocks: u32,
    pub sizes: Vec<String>,
    pub every: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub widget: Info,
    #[serde(default)]
    pub needs: Needs,
}

impl Manifest {
    /// Reads and checks `widget.toml`.
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let m: Manifest = toml::from_str(text).map_err(|e| format!("widget.toml: {e}"))?;
        m.check()?;
        Ok(m)
    }

    /// `author` and `name` from the id.
    pub fn author_and_name(&self) -> (&str, &str) {
        self.widget.id.split_once('/').unwrap_or(("", ""))
    }

    /// The refresh period in seconds.
    pub fn every(&self) -> u64 {
        seconds(&self.widget.every).unwrap_or(MAX_EVERY)
    }

    fn check(&self) -> Result<(), String> {
        let w = &self.widget;
        check_id(&w.id)?;
        if w.name.trim().is_empty() || w.name.chars().count() > 40 {
            return Err("The name must be 1 to 40 characters".into());
        }
        if w.about.chars().count() > 120 {
            return Err("About must be 120 characters at most".into());
        }
        check_version(&w.version)?;
        if w.blocks == 0 {
            return Err("blocks must be 1 or more".into());
        }
        if w.blocks > BLOCKS {
            return Err(format!(
                "It needs blocks version {}; this Arakun draws version {BLOCKS}. It needs a newer Arakun.",
                w.blocks
            ));
        }
        if w.sizes.is_empty() {
            return Err("sizes must name at least one size".into());
        }
        for (i, s) in w.sizes.iter().enumerate() {
            if !SIZES.contains(&s.as_str()) {
                return Err(format!("Unknown size `{s}`: use {}", SIZES.join(", ")));
            }
            if w.sizes[..i].contains(s) {
                return Err(format!("The size `{s}` is listed twice"));
            }
        }
        check_every(&w.every)?;
        self.needs.check()
    }
}

impl Needs {
    fn check(&self) -> Result<(), String> {
        for (i, n) in self.net.iter().enumerate() {
            check_host(&n.host)?;
            check_why(&n.why, &n.host)?;
            check_every(&n.every)?;
            if self.net[..i].iter().any(|o| o.host == n.host) {
                return Err(format!("The host {} is listed twice", n.host));
            }
        }
        for (i, f) in self.file.iter().enumerate() {
            check_file(&f.path)?;
            check_why(&f.why, &f.path)?;
            if self.file[..i].iter().any(|o| o.path == f.path) {
                return Err(format!("The file {} is listed twice", f.path));
            }
        }
        for (i, s) in self.secret.iter().enumerate() {
            check_name(&s.name, "A secret's name")?;
            if s.label.trim().is_empty() {
                return Err(format!("The secret {} needs a label", s.name));
            }
            if !self.net.iter().any(|n| n.host == s.host) {
                return Err(format!(
                    "The secret {} is for {}, which is not in [[needs.net]]",
                    s.name, s.host
                ));
            }
            check_header(&s.header)?;
            if s.format.matches("{}").count() != 1 {
                return Err(format!("The secret {}'s format needs one {{}}", s.name));
            }
            if let Some(url) = &s.get_one
                && !url.starts_with("https://")
            {
                return Err(format!("The secret {}'s get_one must be https", s.name));
            }
            if self.secret[..i].iter().any(|o| o.name == s.name) {
                return Err(format!("The secret {} is listed twice", s.name));
            }
        }
        for (i, a) in self.action.iter().enumerate() {
            if !ACTIONS.contains(&a.kind.as_str()) {
                return Err(format!(
                    "Unknown action `{}`: use {}",
                    a.kind,
                    ACTIONS.join(", ")
                ));
            }
            check_why(&a.why, &a.kind)?;
            if self.action[..i].iter().any(|o| o.kind == a.kind) {
                return Err(format!("The action {} is listed twice", a.kind));
            }
        }
        Ok(())
    }

    pub fn net(&self, host: &str) -> Option<&Net> {
        self.net.iter().find(|n| n.host == host)
    }

    pub fn secret(&self, name: &str) -> Option<&SecretNeed> {
        self.secret.iter().find(|s| s.name == name)
    }

    pub fn allows_action(&self, kind: &str) -> bool {
        self.action.iter().any(|a| a.kind == kind)
    }

    /// The "It can" lines of the Needs step, in plain words, each with its
    /// reason as the author wrote it.
    pub fn lines(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for n in &self.net {
            let every = seconds(&n.every).map(said).unwrap_or_default();
            out.push((format!("Reach {} {every}", n.host), n.why.clone()));
        }
        for f in &self.file {
            out.push((format!("Read {} in this project", f.path), f.why.clone()));
        }
        for s in &self.secret {
            out.push((
                format!("Use your {}, only with {}", s.label, s.host),
                "Arakun adds it to the request. The widget never sees it.".into(),
            ));
        }
        if self.git {
            out.push((
                "See the branch, the commit and the remote".into(),
                "Nothing else from Git".into(),
            ));
        }
        if self.sessions {
            out.push((
                "See the title and state of this project's sessions".into(),
                "Never their messages".into(),
            ));
        }
        if self.notices {
            out.push((
                "Show notices".into(),
                "At most one a minute, with its name".into(),
            ));
        }
        for a in &self.action {
            let what = match a.kind.as_str() {
                "start_session" => "Start a session, after asking you",
                _ => "Open an address, after asking you",
            };
            out.push((what.into(), a.why.clone()));
        }
        out
    }
}

/// The "It can't" lines: the same for every widget, because there is no
/// function for any of them.
pub const CANT: [&str; 6] = [
    "Run commands",
    "Write or delete files",
    "Read your sessions' messages",
    "See your tokens or other secrets",
    "Read the clipboard",
    "Reach an address it did not name",
];

/// What the user allowed: the needs, and how often the widget refreshes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub every: String,
    #[serde(default)]
    pub needs: Needs,
}

impl Grant {
    pub fn of(m: &Manifest) -> Grant {
        Grant {
            every: m.widget.every.clone(),
            needs: m.needs.clone(),
        }
    }

    /// What `next` asks for beyond this grant, one plain line each. Empty:
    /// it may install without asking again.
    pub fn lacks(&self, next: &Grant) -> Vec<String> {
        let (old, new) = (&self.needs, &next.needs);
        let mut more = Vec::new();
        let shorter = |a: &str, b: &str| match (seconds(a), seconds(b)) {
            (Some(a), Some(b)) => b < a,
            _ => true,
        };
        if shorter(&self.every, &next.every) {
            more.push(format!("Refresh every {}", next.every));
        }
        for n in &new.net {
            match old.net(&n.host) {
                None => more.push(format!("Reach {}", n.host)),
                Some(o) if shorter(&o.every, &n.every) => {
                    more.push(format!("Reach {} every {}", n.host, n.every))
                }
                Some(_) => {}
            }
        }
        for f in &new.file {
            if !old.file.iter().any(|o| o.path == f.path) {
                more.push(format!("Read {}", f.path));
            }
        }
        for s in &new.secret {
            match old.secret(&s.name) {
                Some(o) if o.host == s.host && o.header == s.header && o.format == s.format => {}
                _ => more.push(format!("Use your {} with {}", s.label, s.host)),
            }
        }
        for (on, was, what) in [
            (new.git, old.git, "See the branch and commit"),
            (new.sessions, old.sessions, "See this project's sessions"),
            (new.notices, old.notices, "Show notices"),
        ] {
            if on && !was {
                more.push(what.into());
            }
        }
        for a in &new.action {
            if !old.allows_action(&a.kind) {
                more.push(match a.kind.as_str() {
                    "start_session" => "Start sessions".into(),
                    _ => "Open addresses".into(),
                });
            }
        }
        more
    }
}

/// "every 30 s", "every 5 min".
fn said(secs: u64) -> String {
    match secs {
        s if s % 3600 == 0 => format!("every {} h", s / 3600),
        s if s % 60 == 0 => format!("every {} min", s / 60),
        s => format!("every {s} s"),
    }
}

/// `"30s"`, `"5m"` or `"1h"` in seconds.
pub fn seconds(every: &str) -> Option<u64> {
    let every = every.trim();
    let (n, unit) = every.split_at(every.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = n.parse().ok()?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return None,
    };
    n.checked_mul(mult)
}

fn check_every(every: &str) -> Result<(), String> {
    match seconds(every) {
        None => Err(format!("`{every}` is not a period: write 30s, 5m or 1h")),
        Some(s) if s < MIN_EVERY => Err(format!("every is {every}; the shortest is {MIN_EVERY}s")),
        Some(s) if s > MAX_EVERY => Err(format!("every is {every}; the longest is 24h")),
        Some(_) => Ok(()),
    }
}

fn check_why(why: &str, of: &str) -> Result<(), String> {
    match why.trim().chars().count() {
        0 => Err(format!("{of} needs a why")),
        n if n > 120 => Err(format!("The why for {of} is longer than 120 characters")),
        _ => Ok(()),
    }
}

/// Lower case letters, digits and `-`, not at the ends.
fn check_name(name: &str, what: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{what} `{name}` must be lower case letters, digits and -"
        ))
    }
}

/// `<author>/<name>`, each safe as a folder name.
pub fn check_id(id: &str) -> Result<(), String> {
    let Some((author, name)) = id.split_once('/') else {
        return Err(format!("The id `{id}` must be author/name"));
    };
    check_name(author, "The author")?;
    check_name(name, "The name in the id")
}

/// Semver: three numbers, and an optional `-pre` part.
pub fn check_version(v: &str) -> Result<(), String> {
    let core = v
        .split_once('-')
        .map_or(v, |(c, pre)| if pre.is_empty() { "" } else { c });
    let parts: Vec<&str> = core.split('.').collect();
    let ok = parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
        })
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(format!("The version `{v}` is not semver, like 0.4.1"))
    }
}

/// An exact public host name: no wildcard, no address, no port.
pub fn check_host(host: &str) -> Result<(), String> {
    let refuse = |why: &str| Err(format!("The host `{host}` {why}"));
    if host.parse::<IpAddr>().is_ok() || host.starts_with('[') {
        return refuse("is an address; name a host");
    }
    if host.contains('*') {
        return refuse("has a wildcard; name each host");
    }
    if host != host.to_ascii_lowercase() {
        return refuse("must be lower case");
    }
    let labels: Vec<&str> = host.split('.').collect();
    let label_ok = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    if host.len() > 253 || labels.len() < 2 || !labels.iter().all(label_ok) {
        return refuse("is not a host name");
    }
    // Names that only mean something on this computer or this network.
    let last = labels[labels.len() - 1];
    if ["localhost", "local", "internal", "lan", "home", "arpa"].contains(&last)
        || last.bytes().all(|b| b.is_ascii_digit())
    {
        return refuse("is not on the public internet");
    }
    Ok(())
}

/// A path inside the project, written with `/`, that is not a secret file.
pub fn check_file(path: &str) -> Result<(), String> {
    let refuse = |why: &str| Err(format!("The file `{path}` {why}"));
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains(':') {
        return refuse("must be a path inside the project, like vercel.json");
    }
    if path
        .split('/')
        .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return refuse("must not have empty parts, . or ..");
    }
    if !Path::new(path)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return refuse("must be a path inside the project");
    }
    if crate::secrets::secret_path(Path::new(path)) {
        return refuse("is a secret file, which no widget may read");
    }
    Ok(())
}

/// A header name a token may go in. Never one that changes where or how the
/// request goes.
fn check_header(header: &str) -> Result<(), String> {
    let ok = !header.is_empty()
        && header
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let lower = header.to_ascii_lowercase();
    if !ok || FORBIDDEN_HEADERS.contains(&lower.as_str()) {
        return Err(format!("A token can't go in the header `{header}`"));
    }
    Ok(())
}

/// Headers a widget may not set, nor put a token in.
pub const FORBIDDEN_HEADERS: [&str; 9] = [
    "cookie",
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "proxy-authorization",
    "upgrade",
    "te",
    "trailer",
];

/// A widget's files by name.
pub type Files = BTreeMap<String, Vec<u8>>;

/// Checks the set of files: the three required, nothing else, small enough.
pub fn check_files(files: &Files) -> Result<(), String> {
    for name in REQUIRED {
        if !files.contains_key(name) {
            return Err(format!("{name} is missing"));
        }
    }
    if let Some(name) = files
        .keys()
        .find(|n| !REQUIRED.contains(&n.as_str()) && !OPTIONAL.contains(&n.as_str()))
    {
        return Err(format!(
            "{name} is not allowed: a widget has only {} and {}",
            REQUIRED.join(", "),
            OPTIONAL.join(", ")
        ));
    }
    let size: usize = files.values().map(Vec::len).sum();
    if size > MAX_FILES {
        return Err(format!(
            "The files take {} KB; a widget may take 512 KB",
            size.div_ceil(1024)
        ));
    }
    Ok(())
}

/// The SHA-256 the index pins for a version: over the files sorted by name,
/// each as its name, a zero byte, its length (8 bytes, little endian) and
/// its bytes. Lowercase hex.
pub fn hash(files: &Files) -> String {
    let mut h = Sha256::new();
    for (name, bytes) in files {
        h.update(name.as_bytes());
        h.update([0]);
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
pub(super) const DEPLOY_BOARD: &str = r#"
[widget]
id = "mrvn/deploy-board"
name = "Deploy board"
about = "Vercel deploys and their checks, live"
version = "0.4.1"
license = "MIT"
blocks = 1
sizes = ["wide", "tab"]
every = "30s"

[needs]
git = true
sessions = false
notices = false

[[needs.net]]
host = "api.vercel.com"
why = "Deploys for your project"
every = "30s"

[[needs.net]]
host = "api.github.com"
why = "Checks on this repository. Nothing else."
every = "60s"

[[needs.file]]
path = "vercel.json"
why = "In this project only, to find the project id"

[[needs.secret]]
name = "vercel"
label = "Vercel token"
host = "api.vercel.com"
header = "Authorization"
format = "Bearer {}"
get_one = "https://vercel.com/account/tokens"

[[needs.action]]
kind = "start_session"
why = "Start a session with the build log"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn with(edit: impl Fn(&mut Manifest)) -> Result<(), String> {
        let mut m = Manifest::parse(DEPLOY_BOARD).unwrap();
        edit(&mut m);
        m.check()
    }

    #[test]
    fn the_spec_example_reads() {
        let m = Manifest::parse(DEPLOY_BOARD).unwrap();
        assert_eq!(m.author_and_name(), ("mrvn", "deploy-board"));
        assert_eq!(m.every(), 30);
        assert_eq!(m.needs.net.len(), 2);
        assert!(m.needs.git);
        assert_eq!(m.needs.secret("vercel").unwrap().host, "api.vercel.com");
        let lines = m.needs.lines();
        assert_eq!(lines[0].0, "Reach api.vercel.com every 30 s");
        assert_eq!(lines[1].0, "Reach api.github.com every 1 min");
    }

    #[test]
    fn unknown_keys_are_refused() {
        let text = DEPLOY_BOARD.replace("git = true", "git = true\nshell = true");
        assert!(Manifest::parse(&text).is_err());
    }

    #[test]
    fn hosts_must_be_exact_and_public() {
        for bad in [
            "*.vercel.com",
            "127.0.0.1",
            "[::1]",
            "localhost",
            "printer.local",
            "API.vercel.com",
            "vercel",
            "a..b",
            "db.internal",
            "1.2.3.4.5",
        ] {
            assert!(check_host(bad).is_err(), "{bad}");
        }
        assert!(check_host("api.vercel.com").is_ok());
    }

    #[test]
    fn files_stay_in_the_project_and_away_from_secrets() {
        for bad in [
            "/etc/passwd",
            "../x",
            "a/../b",
            "./a",
            ".env",
            "config/.env.local",
            "C:/x",
            "a\\b",
            "",
        ] {
            assert!(check_file(bad).is_err(), "{bad}");
        }
        assert!(check_file("vercel.json").is_ok());
        assert!(check_file("config/app.json").is_ok());
    }

    #[test]
    fn rules_for_the_widget_itself() {
        assert!(with(|m| m.widget.every = "10s".into()).is_err());
        assert!(with(|m| m.widget.every = "soon".into()).is_err());
        assert!(
            with(|m| m.widget.blocks = 2)
                .unwrap_err()
                .contains("newer Arakun")
        );
        assert!(with(|m| m.widget.sizes = vec!["huge".into()]).is_err());
        assert!(with(|m| m.widget.sizes = vec![]).is_err());
        assert!(with(|m| m.widget.id = "mrvn".into()).is_err());
        assert!(with(|m| m.widget.id = "../x/y".into()).is_err());
        assert!(with(|m| m.widget.version = "1.0".into()).is_err());
        assert!(with(|m| m.widget.version = "01.0.0".into()).is_err());
        assert!(with(|m| m.widget.version = "1.0.0-beta.1".into()).is_ok());
    }

    #[test]
    fn rules_for_needs() {
        // A token for a host it does not reach.
        assert!(with(|m| m.needs.secret[0].host = "evil.example.com".into()).is_err());
        assert!(with(|m| m.needs.secret[0].header = "Cookie".into()).is_err());
        assert!(with(|m| m.needs.secret[0].format = "Bearer".into()).is_err());
        assert!(with(|m| m.needs.action[0].kind = "run_command".into()).is_err());
        assert!(with(|m| m.needs.net[0].why = " ".into()).is_err());
        assert!(with(|m| m.needs.net[1].host = "api.vercel.com".into()).is_err());
    }

    #[test]
    fn an_update_that_asks_for_more_is_caught() {
        let m = Manifest::parse(DEPLOY_BOARD).unwrap();
        let old = Grant::of(&m);
        assert!(old.lacks(&old).is_empty());

        let mut fewer = old.clone();
        fewer.needs.net.pop();
        fewer.needs.git = false;
        assert!(old.lacks(&fewer).is_empty());

        let mut more = old.clone();
        more.needs.net.push(Net {
            host: "api.sentry.io".into(),
            why: "x".into(),
            every: "60s".into(),
        });
        more.needs.net[1].every = "30s".into();
        more.needs.sessions = true;
        more.needs.secret[0].host = "api.github.com".into();
        more.every = "30s".into();
        assert_eq!(
            old.lacks(&more),
            [
                "Reach api.github.com every 30s",
                "Reach api.sentry.io",
                "Use your Vercel token with api.github.com",
                "See this project's sessions",
            ]
        );
        let mut faster = old.clone();
        faster.every = "29s".into();
        assert_eq!(old.lacks(&faster), ["Refresh every 29s"]);
    }

    #[test]
    fn the_file_set_is_fixed() {
        let mut files = Files::new();
        for n in REQUIRED {
            files.insert(n.into(), b"x".to_vec());
        }
        assert!(check_files(&files).is_ok());
        files.insert("run.sh".into(), Vec::new());
        assert!(check_files(&files).is_err());
        files.remove("run.sh");
        files.insert("README.md".into(), vec![0; MAX_FILES]);
        assert!(check_files(&files).is_err());
        files.remove("main.rhai");
        assert!(check_files(&files).unwrap_err().contains("main.rhai"));
    }

    #[test]
    fn the_hash_covers_names_and_bytes() {
        let mut a = Files::new();
        a.insert("a".into(), b"bc".to_vec());
        let mut b = Files::new();
        b.insert("ab".into(), b"c".to_vec());
        assert_ne!(hash(&a), hash(&b));
        assert_eq!(hash(&a).len(), 64);
    }
}
