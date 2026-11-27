//! Mirrors what this computer installed from the marketplace onto paired
//! servers: Claude Code plugins (and the marketplaces they come from), MCP
//! servers added for the user, skills from skills.sh, and the MCP sign-ins
//! Claude keeps. It only adds: what a server has already, or has that this
//! computer does not, is left alone.
//!
//! The app sends a [`Snapshot`] when a server connects and after each install
//! here (`ClientMsg::Mirror`); the server applies it with [`apply`] and says
//! what it added (`mirror_done`). Skills go by where skills.sh found them,
//! not as files: the server copies them from GitHub itself.
//!
//! Discover also installs for one repository ([`Where`]). What it put there
//! for you alone is kept in a list with the repository's `origin`, and a
//! server adds it in its own checkouts of that repository. What it shared
//! with the repository is in its files, and goes with git.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};

use super::launch;
use super::skills_sh::{self, SkillEntry};

const CLI_TIMEOUT: Duration = Duration::from_secs(30);
const MARKETPLACE_TIMEOUT: Duration = Duration::from_secs(180);
/// Where macOS keeps Claude Code's sign-ins.
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// What this computer has, for a server to add what it lacks. Its `Debug`
/// names the MCP servers and sign-ins but never shows what they hold.
#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    /// User-scope plugin ids, `name@marketplace`.
    #[serde(default)]
    pub plugins: Vec<String>,
    /// The marketplaces those plugins come from, by name: their `source` as
    /// Claude Code records it.
    #[serde(default)]
    pub marketplaces: BTreeMap<String, Value>,
    /// User-scope MCP servers, by name, as in `~/.claude.json`. They can hold
    /// keys, so only a server reached safely gets them.
    #[serde(default)]
    pub servers: BTreeMap<String, Value>,
    /// Skills installed from skills.sh.
    #[serde(default)]
    pub skills: Vec<SkillEntry>,
    /// Claude Code's MCP sign-ins (`mcpOAuth`), by its own keys. Secret, like `servers`.
    #[serde(default)]
    pub oauth: Map<String, Value>,
    /// What was installed for you alone in a repository, by repository.
    #[serde(default)]
    pub repos: Vec<RepoMirror>,
}

/// What one repository has for you alone (Claude Code's `local` scope).
#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RepoMirror {
    /// [`crate::clone_source::repo_key`] of its `origin`.
    pub repo: String,
    #[serde(default)]
    pub plugins: Vec<String>,
    /// Secret, like [`Snapshot::servers`].
    #[serde(default)]
    pub servers: BTreeMap<String, Value>,
    #[serde(default)]
    pub skills: Vec<SkillEntry>,
}

/// The names in a map whose values hold keys, for a `Debug` print: a
/// [`crate::secret::Secret`] takes a string, not a JSON tree.
fn names<'a>(m: impl IntoIterator<Item = (&'a String, &'a Value)>) -> Vec<&'a str> {
    m.into_iter().map(|(k, _)| k.as_str()).collect()
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("plugins", &self.plugins)
            .field("marketplaces", &self.marketplaces)
            .field("servers", &names(&self.servers))
            .field("skills", &self.skills)
            .field("oauth", &names(&self.oauth))
            .field("repos", &self.repos)
            .finish()
    }
}

impl std::fmt::Debug for RepoMirror {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepoMirror")
            .field("repo", &self.repo)
            .field("plugins", &self.plugins)
            .field("servers", &names(&self.servers))
            .field("skills", &self.skills)
            .finish()
    }
}

impl RepoMirror {
    fn is_empty(&self) -> bool {
        self.plugins.is_empty() && self.servers.is_empty() && self.skills.is_empty()
    }
}

/// Where Discover installs on this computer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Where {
    /// For the user, in every repository.
    #[default]
    User,
    /// In this repository's files, for everyone who clones it.
    Shared,
    /// In this repository, for you alone: kept out of git.
    Mine,
}

impl Where {
    pub const ALL: [Where; 3] = [Where::User, Where::Shared, Where::Mine];

    /// Claude Code's `--scope` for it.
    pub fn claude(self) -> &'static str {
        match self {
            Where::User => "user",
            Where::Shared => "project",
            Where::Mine => "local",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Where::User => "Every repo",
            Where::Shared => "This repo, shared",
            Where::Mine => "This repo, just me",
        }
    }

    /// The end of a notice that it was installed.
    pub fn done(self) -> &'static str {
        match self {
            Where::User => "for every project",
            Where::Shared => "in this repo's files, for everyone who clones it",
            Where::Mine => "in this repo, for you alone",
        }
    }
}

impl Snapshot {
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
            && self.servers.is_empty()
            && self.skills.is_empty()
            && self.oauth.is_empty()
            && self.repos.is_empty()
    }

    /// The `mirror` message that carries it.
    pub fn message(&self) -> Value {
        let mut msg = serde_json::to_value(self).unwrap_or_else(|_| json!({}));
        msg["type"] = json!("mirror");
        msg
    }
}

// ---------------------------------------------------------------------------
// On this computer.

/// Blocking: what this computer has. Without `secrets`, for a server reached
/// over plain http, it leaves out the MCP servers and the sign-ins.
pub fn snapshot(secrets: bool) -> Snapshot {
    let home = launch::home();
    let plugins_dir = home.join(".claude").join("plugins");
    let plugins = user_plugins(&read_json(&plugins_dir.join("installed_plugins.json")));
    let state = read_json(&home.join(".claude.json"));
    let repos = repo_mirrors(&remembered_repo_installs(), &state, secrets);
    let every_plugin: Vec<String> = plugins
        .iter()
        .chain(repos.iter().flat_map(|r| r.plugins.iter()))
        .cloned()
        .collect();
    let marketplaces = needed_marketplaces(
        &every_plugin,
        &read_json(&plugins_dir.join("known_marketplaces.json")),
    );
    let skills = remembered_skills()
        .into_iter()
        .filter(|s| skill_dir(&skills_sh::skills_root(), s).is_some_and(|d| d.is_dir()))
        .collect();
    let (servers, oauth) = if secrets {
        let servers = state["mcpServers"]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let oauth = read_credentials()["mcpOAuth"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        (servers, oauth)
    } else {
        Default::default()
    };
    Snapshot {
        plugins,
        marketplaces,
        servers,
        skills,
        oauth,
        repos,
    }
}

/// Blocking: the `/` commands of what [`snapshot`] sends every server, the
/// user's plugins and skills.sh skills. A server's session shows them before
/// it says what it has.
pub fn mirrored_commands() -> Vec<super::Choice> {
    let home = launch::home();
    let plugins = user_plugins(&read_json(
        &home.join(".claude/plugins/installed_plugins.json"),
    ));
    let root = skills_sh::skills_root();
    let dirs: Vec<PathBuf> = remembered_skills()
        .iter()
        .filter_map(|s| skill_dir(&root, s))
        .collect();
    super::claude_ext::mirrored_choices(&plugins, &dirs)
}

/// What the list of installs for you alone still holds, by repository. An
/// MCP server goes only with `secrets`.
fn repo_mirrors(kept: &[RepoInstall], state: &Value, secrets: bool) -> Vec<RepoMirror> {
    let mut by_repo: BTreeMap<String, RepoMirror> = BTreeMap::new();
    for k in kept.iter().filter(|k| !k.shared) {
        let Some(repo) = &k.repo else { continue };
        let m = by_repo.entry(repo.clone()).or_insert_with(|| RepoMirror {
            repo: repo.clone(),
            ..Default::default()
        });
        match &k.item {
            RepoItem::Plugin { id } => {
                let local = read_json(&k.path.join(".claude").join("settings.local.json"));
                if local["enabledPlugins"][id].as_bool() == Some(true) && !m.plugins.contains(id) {
                    m.plugins.push(id.clone());
                }
            }
            RepoItem::Server { name } if secrets => {
                let config = &super::claude_ext::project_entry(state, &k.path)["mcpServers"][name];
                if config.is_object() {
                    m.servers.insert(name.clone(), config.clone());
                }
            }
            RepoItem::Server { .. } => {}
            RepoItem::Skill { entry } => {
                let there = skill_dir(&repo_skills(&k.path), entry).is_some_and(|d| d.is_dir());
                if there && !m.skills.iter().any(|s| s.skill_id == entry.skill_id) {
                    m.skills.push(entry.clone());
                }
            }
        }
    }
    by_repo.into_values().filter(|m| !m.is_empty()).collect()
}

fn read_json(path: &Path) -> Value {
    crate::durable::load_json(path).unwrap_or(Value::Null)
}

/// The plugin ids installed for the user, sorted.
fn user_plugins(installed: &Value) -> Vec<String> {
    let mut ids: Vec<String> = installed["plugins"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, records)| {
            records
                .as_array()
                .into_iter()
                .flatten()
                .any(|r| r["scope"] == "user")
        })
        .map(|(id, _)| id.clone())
        .collect();
    ids.sort();
    ids
}

/// The marketplaces `plugins` come from that Claude Code knows a source for.
/// One it adds by itself (the official one) has none and needs none.
fn needed_marketplaces(plugins: &[String], known: &Value) -> BTreeMap<String, Value> {
    plugins
        .iter()
        .filter_map(|id| id.rsplit_once('@').map(|(_, m)| m))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|m| {
            let source = &known[m]["source"];
            source.is_object().then(|| (m.to_string(), source.clone()))
        })
        .collect()
}

/// The skills.sh skills installed here, as [`remember_skill`] kept them.
fn skills_file() -> PathBuf {
    crate::paths::shared(crate::paths::SKILLS_INSTALLED)
}

fn remembered_skills() -> Vec<SkillEntry> {
    crate::durable::load_json(skills_file()).unwrap_or_default()
}

/// Keeps a skill installed from skills.sh, so it is mirrored to servers.
pub fn remember_skill(entry: &SkillEntry) {
    let mut all = remembered_skills();
    all.retain(|s| s.skill_id != entry.skill_id);
    all.push(entry.clone());
    if let Err(e) = crate::durable::save_json(&skills_file(), &all) {
        eprintln!("arakun: could not keep the skills installed from skills.sh: {e}");
    }
}

/// Something Discover installed in one repository.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RepoInstall {
    /// The folder it was installed in, on this computer.
    pub path: PathBuf,
    /// [`crate::clone_source::repo_key`] of its `origin`: none without one.
    pub repo: Option<String>,
    /// In the repository's files rather than for you alone.
    pub shared: bool,
    pub item: RepoItem,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RepoItem {
    Plugin { id: String },
    Server { name: String },
    Skill { entry: SkillEntry },
}

fn repo_installs_file() -> PathBuf {
    crate::paths::shared(crate::paths::REPO_INSTALLS)
}

fn remembered_repo_installs() -> Vec<RepoInstall> {
    crate::durable::load_json(repo_installs_file()).unwrap_or_default()
}

/// Blocking (asks git for the `origin`): keeps what Discover installed in the
/// repository at `dir`, so it is mirrored to servers.
pub fn remember_repo_install(dir: &Path, shared: bool, item: RepoItem) {
    let mut all = remembered_repo_installs();
    all.retain(|k| !(k.path == dir && k.item == item));
    all.push(RepoInstall {
        path: dir.to_path_buf(),
        repo: repo_of(dir),
        shared,
        item,
    });
    if let Err(e) = crate::durable::save_json(&repo_installs_file(), &all) {
        eprintln!(
            "arakun: could not keep what was installed in {}: {e}",
            dir.display()
        );
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    // The folder picks the repository, not one inherited from a git hook.
    let mut cmd = crate::git::git(dir);
    cmd.args(args);
    let out = crate::platform::output_within(&mut cmd, None, crate::platform::GIT_LOCAL).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Blocking: what the repository at `dir` is known by on every machine.
pub fn repo_of(dir: &Path) -> Option<String> {
    crate::clone_source::repo_key(&git(dir, &["remote", "get-url", "origin"])?)
}

/// Blocking: what an MCP server installed in the repository at `dir` has
/// after its name, so it has a sign-in of its own there: `acme-web` for
/// acme/web. The owner is in it, so acme/web and other/web differ.
pub fn repo_slug(dir: &Path) -> String {
    let repo = repo_of(dir)
        .map(|r| {
            let mut parts: Vec<&str> = r.trim_end_matches('/').rsplit('/').take(2).collect();
            parts.reverse();
            parts.join("-")
        })
        .or_else(|| dir.file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_default();
    repo.trim_end_matches(".git").to_string()
}

/// `name` with the repository's [`repo_slug`] after it: `github-acme-web`.
pub fn suffixed(name: &str, repo: &str) -> String {
    let slug: String = repo
        .trim_end_matches(".git")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        name.to_string()
    } else {
        format!("{name}-{slug}")
    }
}

/// What a repository has already, so Discover does not offer it again.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepoHas {
    pub dir: PathBuf,
    /// [`repo_slug`], for the name of a server that signs in.
    pub slug: String,
    pub shared_plugins: BTreeSet<String>,
    pub my_plugins: BTreeSet<String>,
    pub shared_servers: BTreeSet<String>,
    pub my_servers: BTreeSet<String>,
    /// Folder names in its `.claude/skills`, lower case.
    pub skills: BTreeSet<String>,
}

impl RepoHas {
    pub fn plugin(&self, at: Where, id: &str) -> bool {
        match at {
            Where::Shared => self.shared_plugins.contains(id),
            Where::Mine => self.my_plugins.contains(id),
            Where::User => false,
        }
    }

    pub fn server(&self, at: Where, name: &str) -> bool {
        match at {
            Where::Shared => self.shared_servers.contains(name),
            Where::Mine => self.my_servers.contains(name),
            Where::User => false,
        }
    }

    pub fn skill(&self, skill_id: &str) -> bool {
        self.skills.contains(&skill_id.to_lowercase())
    }
}

/// Blocking: what the repository at `dir` has, in its files and for you.
pub fn repo_has(dir: &Path) -> RepoHas {
    let on = |v: &Value| -> BTreeSet<String> {
        v["enabledPlugins"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, on)| on.as_bool() == Some(true))
            .map(|(id, _)| id.clone())
            .collect()
    };
    let names = |v: &Value| -> BTreeSet<String> {
        v.as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    };
    let claude = dir.join(".claude");
    let state = read_json(&launch::home().join(".claude.json"));
    RepoHas {
        dir: dir.to_path_buf(),
        slug: repo_slug(dir),
        shared_plugins: on(&read_json(&claude.join("settings.json"))),
        my_plugins: on(&read_json(&claude.join("settings.local.json"))),
        shared_servers: names(&read_json(&dir.join(".mcp.json"))["mcpServers"]),
        my_servers: names(&super::claude_ext::project_entry(&state, dir)["mcpServers"]),
        skills: std::fs::read_dir(claude.join("skills"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_lowercase())
            .collect(),
    }
}

/// A repository's own skills folder.
fn repo_skills(dir: &Path) -> PathBuf {
    dir.join(".claude").join("skills")
}

/// Blocking: copies a skills.sh skill where `at` says, in the repository at
/// `dir` unless it is for the user. One for you alone is kept out of git.
pub fn install_skill(
    entry: &SkillEntry,
    found: &skills_sh::SkillSource,
    at: Where,
    dir: &Path,
) -> Result<()> {
    let dest = skills_sh::install(entry, found, &skills_root_for(at, dir))?;
    if at == Where::User {
        remember_skill(entry);
        return Ok(());
    }
    // Left in place, it would go into git, and a second Install would take
    // the folder for done: it goes, and Install can run again.
    if at == Where::Mine
        && let Err(e) = keep_out_of_git(dir, &dest)
    {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(e);
    }
    remember_repo_install(
        dir,
        at == Where::Shared,
        RepoItem::Skill {
            entry: entry.clone(),
        },
    );
    Ok(())
}

/// Where `install_skill` puts a skill for `at`.
fn skills_root_for(at: Where, dir: &Path) -> PathBuf {
    match at {
        Where::User => skills_sh::skills_root(),
        Where::Shared | Where::Mine => repo_skills(dir),
    }
}

/// Blocking: installs every skill of a package where `at` says, as
/// [`install_skill`] does one. A skill whose folder is there already is left as
/// it is; one that fails does not stop the others. Returns how many it installed.
pub fn install_package(pkg: &skills_sh::Package, at: Where, dir: &Path) -> Result<usize> {
    let root = skills_root_for(at, dir);
    let mut done = 0;
    let mut failed = Vec::new();
    for (entry, found) in &pkg.skills {
        if skill_dir(&root, entry).is_some_and(|d| d.exists()) {
            continue;
        }
        match install_skill(entry, found, at, dir) {
            Ok(()) => done += 1,
            Err(e) => failed.push(format!("{}: {e:#}", entry.skill_id)),
        }
    }
    if !failed.is_empty() {
        bail!(
            "{done} installed, {} did not ({})",
            failed.len(),
            failed.join("; ")
        );
    }
    Ok(done)
}

/// Lists `path` in the repository's `info/exclude`, which is not committed.
fn keep_out_of_git(dir: &Path, path: &Path) -> Result<()> {
    let Ok(rel) = path.strip_prefix(dir) else {
        bail!("{} is not in {}", path.display(), dir.display());
    };
    let line = format!("/{}/", rel.to_string_lossy().replace('\\', "/"));
    let exclude = git(dir, &["rev-parse", "--git-path", "info/exclude"])
        .ok_or_else(|| anyhow!("{} is not in a git repository", dir.display()))?;
    let exclude = dir.join(exclude);
    let old = std::fs::read_to_string(&exclude).unwrap_or_default();
    if old.lines().any(|l| l.trim() == line) {
        return Ok(());
    }
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let sep = if old.is_empty() || old.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    crate::durable::write(&exclude, format!("{old}{sep}{line}\n").as_bytes())?;
    Ok(())
}

fn skill_dir(root: &Path, entry: &SkillEntry) -> Option<PathBuf> {
    skills_sh::folder_name(&entry.skill_id)
        .ok()
        .map(|n| root.join(n))
}

fn credentials_file() -> PathBuf {
    crate::platform::home()
        .join(".claude")
        .join(".credentials.json")
}

/// Claude Code's sign-ins: the Keychain on macOS (the file when that has none),
/// the file elsewhere.
fn read_credentials() -> Value {
    #[cfg(target_os = "macos")]
    {
        let found = super::claude_ext::run_cli(
            Path::new("/usr/bin/security"),
            &launch::home(),
            &["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"],
            CLI_TIMEOUT,
        );
        if let Ok(v) = found
            .as_deref()
            .map(str::trim)
            .map(serde_json::from_str::<Value>)
            .unwrap_or(Ok(Value::Null))
            && v.is_object()
        {
            return v;
        }
    }
    read_json(&credentials_file())
}

// ---------------------------------------------------------------------------
// On a server.

/// One mirror at a time: two at once would install the same plugin twice.
static APPLYING: Mutex<()> = Mutex::new(());

/// Blocking: adds what `s` has and this machine lacks, and says what it added
/// and what failed (`mirror_done`). What is for one repository goes into
/// each of `places` (the server's folders) that is a checkout of it.
pub fn apply(s: Snapshot, places: &[PathBuf]) -> Value {
    let _one = APPLYING.lock().unwrap_or_else(|e| e.into_inner());
    let home = launch::home();
    let mut added = Vec::new();
    let mut failed = Vec::new();
    let mut note = |name: String, r: Result<()>| match r {
        Ok(()) => added.push(name),
        Err(e) => failed.push(json!({ "name": name, "error": format!("{e:#}") })),
    };

    let plugins_dir = home.join(".claude").join("plugins");
    let have_plugins: BTreeSet<String> =
        user_plugins(&read_json(&plugins_dir.join("installed_plugins.json")))
            .into_iter()
            .collect();
    let missing: Vec<&String> = s
        .plugins
        .iter()
        .filter(|p| !have_plugins.contains(*p))
        .collect();
    add_marketplaces(&home, &missing, &s.marketplaces, &mut note);
    for id in missing {
        note(id.clone(), super::marketplace::install_plugin(&home, id));
    }

    let have_servers = read_json(&home.join(".claude.json"))["mcpServers"].clone();
    for (name, config) in &s.servers {
        if !have_servers[name].is_null() {
            continue;
        }
        let r = runs_here(config).and_then(|()| add_server(&home, name, config, "user"));
        note(name.clone(), r);
    }

    let root = skills_sh::skills_root();
    for entry in &s.skills {
        let Some(dir) = skill_dir(&root, entry) else {
            continue;
        };
        if dir.exists() {
            continue;
        }
        let r = skills_sh::locate(entry)
            .and_then(|found| skills_sh::install(entry, &found, &root))
            .map(|_| remember_skill(entry));
        note(entry.name.clone(), r);
    }

    // Each folder's repository, asked once.
    let checkouts: Vec<(&PathBuf, String)> = if s.repos.is_empty() {
        Vec::new()
    } else {
        places
            .iter()
            .filter_map(|p| repo_of(p).map(|r| (p, r)))
            .collect()
    };
    let state = read_json(&home.join(".claude.json"));
    for r in &s.repos {
        for (dir, _) in checkouts.iter().filter(|(_, repo)| *repo == r.repo) {
            apply_repo(r, dir, &state, &s.marketplaces, &mut note);
        }
    }

    if !s.oauth.is_empty() {
        match write_sign_ins(&s.oauth) {
            Ok(names) => names
                .into_iter()
                .for_each(|n| note(format!("sign-in for {n}"), Ok(()))),
            Err(e) => note("MCP sign-ins".into(), Err(e)),
        }
    }

    json!({ "type": "mirror_done", "added": added, "failed": failed })
}

/// Adds the marketplaces the plugins `ids` come from, when Claude Code here
/// does not know them yet.
fn add_marketplaces(
    home: &Path,
    ids: &[&String],
    sources: &BTreeMap<String, Value>,
    note: &mut impl FnMut(String, Result<()>),
) {
    if ids.is_empty() {
        return;
    }
    let known = read_json(
        &home
            .join(".claude")
            .join("plugins")
            .join("known_marketplaces.json"),
    );
    let wanted: BTreeSet<&str> = ids
        .iter()
        .filter_map(|id| id.rsplit_once('@').map(|(_, m)| m))
        .collect();
    for m in wanted {
        if known[m].is_object() {
            continue;
        }
        let Some(source) = sources.get(m) else {
            continue;
        };
        let r = source_arg(source).and_then(|arg| {
            claude(
                home,
                &["plugin", "marketplace", "add", &arg],
                MARKETPLACE_TIMEOUT,
            )
        });
        if let Err(e) = r {
            note(format!("marketplace {m}"), Err(e));
        }
    }
}

/// Adds what `r` has for you alone to the checkout at `dir`, in Claude Code's
/// `local` scope there.
fn apply_repo(
    r: &RepoMirror,
    dir: &Path,
    state: &Value,
    sources: &BTreeMap<String, Value>,
    note: &mut impl FnMut(String, Result<()>),
) {
    let home = launch::home();
    let folder = dir.file_name().map_or_else(
        || dir.display().to_string(),
        |f| f.to_string_lossy().into_owned(),
    );
    let local = read_json(&dir.join(".claude").join("settings.local.json"));
    let missing: Vec<&String> = r
        .plugins
        .iter()
        .filter(|id| local["enabledPlugins"][id.as_str()].as_bool() != Some(true))
        .collect();
    add_marketplaces(&home, &missing, sources, note);
    for id in missing {
        let done = super::marketplace::install_plugin_as(dir, id, Where::Mine.claude());
        note(format!("{id} in {folder}"), done);
    }
    let have = &super::claude_ext::project_entry(state, dir)["mcpServers"];
    for (name, config) in &r.servers {
        if !have[name].is_null() {
            continue;
        }
        let done = runs_here(config).and_then(|()| add_server(dir, name, config, "local"));
        note(format!("{name} in {folder}"), done);
    }
    let root = repo_skills(dir);
    for entry in &r.skills {
        let Some(dest) = skill_dir(&root, entry) else {
            continue;
        };
        if dest.exists() {
            continue;
        }
        let done = skills_sh::locate(entry)
            .and_then(|found| skills_sh::install(entry, &found, &root))
            .and_then(|dest| keep_out_of_git(dir, &dest));
        note(format!("{} in {folder}", entry.name), done);
    }
}

/// Adds an MCP server with `claude mcp add-json`, its config on stdin: it can
/// hold keys, which the process list would show.
fn add_server(dir: &Path, name: &str, config: &Value, scope: &str) -> Result<()> {
    let bin = launch::which("claude").ok_or_else(|| anyhow!("claude is not on PATH"))?;
    super::claude_ext::run_cli_input(
        &bin,
        dir,
        &["mcp", "add-json", name, "-", "-s", scope],
        config.to_string().as_bytes(),
        CLI_TIMEOUT,
    )
    .map(|_| ())
}

fn claude(home: &Path, args: &[&str], timeout: Duration) -> Result<()> {
    let bin = launch::which("claude").ok_or_else(|| anyhow!("claude is not on PATH"))?;
    super::claude_ext::run_cli(&bin, home, args, timeout).map(|_| ())
}

/// What `claude plugin marketplace add` takes for a recorded source. A folder
/// or file on the other computer is not here.
fn source_arg(source: &Value) -> Result<String> {
    let field = |k: &str| source[k].as_str().map(str::to_string);
    let arg = match source["source"].as_str().unwrap_or("") {
        "github" => field("repo"),
        "git" | "url" => field("url"),
        other => bail!("its source ({other}) is on the other computer"),
    };
    arg.ok_or_else(|| anyhow!("its source names no repository"))
}

/// A server started from a full path runs only where that path is.
fn runs_here(config: &Value) -> Result<()> {
    let Some(cmd) = config["command"].as_str() else {
        return Ok(());
    };
    // A full path of the other kind (`C:\…` here, `/…` on Windows) is not here either.
    let unix = cmd.starts_with('/');
    let windows = cmd.starts_with("\\\\")
        || cmd.as_bytes().get(1) == Some(&b':')
            && cmd.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    let foreign = if cfg!(windows) { unix } else { windows };
    if foreign || Path::new(cmd).is_absolute() && !Path::new(cmd).exists() {
        bail!("it runs {cmd}, which this machine does not have");
    }
    Ok(())
}

/// Adds the sign-ins this machine lacks, or holds an older copy of, to Claude
/// Code's credentials file. Gives the names of the servers they are for.
fn write_sign_ins(incoming: &Map<String, Value>) -> Result<Vec<String>> {
    if cfg!(target_os = "macos") {
        bail!("Claude Code keeps them in the Keychain here, which arakun does not write");
    }
    let path = credentials_file();
    let mut creds = read_json(&path);
    if !creds.is_object() {
        if path.exists() {
            bail!("cannot read {}", path.display());
        }
        creds = json!({});
    }
    let names = merge_sign_ins(&mut creds, incoming);
    if !names.is_empty() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(&creds)?;
        crate::durable::write_private(&path, &bytes)?;
    }
    Ok(names)
}

/// Puts each incoming sign-in into `creds["mcpOAuth"]` when it is missing there
/// or expires later than the one there. Gives the servers they are for.
fn merge_sign_ins(creds: &mut Value, incoming: &Map<String, Value>) -> Vec<String> {
    if !creds["mcpOAuth"].is_object() {
        creds["mcpOAuth"] = json!({});
    }
    let Some(have) = creds["mcpOAuth"].as_object_mut() else {
        return Vec::new();
    };
    let expires = |v: &Value| v["expiresAt"].as_f64().unwrap_or(0.);
    let mut names = Vec::new();
    for (key, token) in incoming {
        if !token.is_object() {
            continue;
        }
        let newer = have
            .get(key)
            .is_none_or(|old| expires(token) > expires(old));
        if newer {
            have.insert(key.clone(), token.clone());
            let name = token["serverName"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| key.split('|').next().unwrap_or(key).to_string());
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_user_plugins_are_mirrored() {
        let installed = json!({ "version": 2, "plugins": {
            "b@m": [{ "scope": "user" }],
            "a@n": [{ "scope": "project" }, { "scope": "user" }],
            "c@m": [{ "scope": "local" }],
        }});
        assert_eq!(user_plugins(&installed), vec!["a@n", "b@m"]);
        assert!(user_plugins(&Value::Null).is_empty());
    }

    #[test]
    fn marketplaces_go_with_the_plugins_that_need_them() {
        let known = json!({
            "m": { "source": { "source": "github", "repo": "o/m" } },
            "x": { "source": { "source": "github", "repo": "o/x" } },
        });
        let plugins = vec!["a@m".to_string(), "b@builtin".to_string()];
        let got = needed_marketplaces(&plugins, &known);
        assert_eq!(got.keys().collect::<Vec<_>>(), vec!["m"]);
    }

    #[test]
    fn a_source_becomes_what_marketplace_add_takes() {
        let gh = json!({ "source": "github", "repo": "o/r" });
        assert_eq!(source_arg(&gh).unwrap(), "o/r");
        let git = json!({ "source": "git", "url": "https://x/y.git" });
        assert_eq!(source_arg(&git).unwrap(), "https://x/y.git");
        assert!(source_arg(&json!({ "source": "directory", "path": "/p" })).is_err());
        assert!(source_arg(&json!({ "source": "github" })).is_err());
    }

    #[test]
    fn a_server_at_a_missing_path_is_not_added() {
        assert!(runs_here(&json!({ "command": "npx", "args": ["x"] })).is_ok());
        assert!(runs_here(&json!({ "type": "http", "url": "https://x" })).is_ok());
        let gone = std::env::temp_dir().join("arakun-no-such-mcp-server");
        assert!(runs_here(&json!({ "command": gone })).is_err());
        // A full path of the other system's kind cannot run here either.
        let other = if cfg!(windows) {
            "/usr/bin/x"
        } else {
            "C:\\tools\\x.exe"
        };
        assert!(runs_here(&json!({ "command": other })).is_err());
    }

    #[test]
    fn sign_ins_are_added_or_renewed_never_made_older() {
        let mut creds = json!({
            "claudeAiOauth": { "accessToken": "keep" },
            "mcpOAuth": {
                "a|1": { "serverName": "a", "accessToken": "old", "expiresAt": 100 },
                "b|2": { "serverName": "b", "accessToken": "new", "expiresAt": 500 },
            }
        });
        let incoming = json!({
            "a|1": { "serverName": "a", "accessToken": "fresh", "expiresAt": 200 },
            "b|2": { "serverName": "b", "accessToken": "stale", "expiresAt": 300 },
            "c|3": { "accessToken": "added" },
        });
        let names = merge_sign_ins(&mut creds, incoming.as_object().unwrap());
        assert_eq!(names, vec!["a", "c"]);
        assert_eq!(creds["mcpOAuth"]["a|1"]["accessToken"], "fresh");
        assert_eq!(creds["mcpOAuth"]["b|2"]["accessToken"], "new");
        assert_eq!(creds["mcpOAuth"]["c|3"]["accessToken"], "added");
        // The account's own sign-in is not touched.
        assert_eq!(creds["claudeAiOauth"]["accessToken"], "keep");
        // Again: nothing is newer now.
        assert!(merge_sign_ins(&mut creds, incoming.as_object().unwrap()).is_empty());
    }

    #[test]
    fn a_package_skips_what_is_there_and_tells_what_failed() {
        let dir = std::env::temp_dir().join(format!("arakun-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".claude/skills/here")).unwrap();
        let entry = |id: &str| SkillEntry {
            id: format!("o/r/{id}"),
            source: "o/r".into(),
            skill_id: id.into(),
            name: id.into(),
            installs: 0,
            description: String::new(),
        };
        let mut pkg = skills_sh::Package {
            source: "o/r".into(),
            skills: vec![(
                entry("here"),
                skills_sh::test_source("here", &[("SKILL.md", 3)]),
            )],
        };
        assert_eq!(install_package(&pkg, Where::Shared, &dir).unwrap(), 0);
        // Too big to copy, so it fails before reaching GitHub.
        pkg.skills.push((
            entry("huge"),
            skills_sh::test_source("huge", &[("a.bin", 1 << 30)]),
        ));
        let e = install_package(&pkg, Where::Shared, &dir).unwrap_err();
        let e = format!("{e:#}");
        assert!(e.starts_with("0 installed, 1 did not (huge: "), "{e}");
        assert!(!dir.join(".claude/skills/huge").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "reaches GitHub"]
    fn a_package_installs_live() {
        let dir = std::env::temp_dir().join(format!("arakun-pkg-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pkg = skills_sh::package("anthropics/skills").unwrap();
        let n = install_package(&pkg, Where::Shared, &dir).unwrap();
        assert_eq!(n, pkg.skills.len());
        assert!(dir.join(".claude/skills/pdf/SKILL.md").is_file());
        // Again, everything is there already.
        assert_eq!(install_package(&pkg, Where::Shared, &dir).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_server_per_repo_is_named_after_the_repo() {
        assert_eq!(suffixed("github", "acme-web"), "github-acme-web");
        assert_eq!(suffixed("github", "My.Repo.git"), "github-my-repo");
        assert_eq!(suffixed("github", ""), "github");
    }

    #[test]
    fn only_installs_for_you_alone_that_are_still_there_are_mirrored() {
        let dir = std::env::temp_dir().join(format!("arakun-mirror-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".claude/skills/kept")).unwrap();
        std::fs::write(
            dir.join(".claude/settings.local.json"),
            r#"{"enabledPlugins": {"on@m": true, "off@m": false}}"#,
        )
        .unwrap();
        let entry = |id: &str| SkillEntry {
            id: format!("o/r/{id}"),
            source: "o/r".into(),
            skill_id: id.into(),
            name: id.into(),
            installs: 0,
            description: String::new(),
        };
        let kept = |shared, repo: Option<&str>, item| RepoInstall {
            path: dir.clone(),
            repo: repo.map(str::to_string),
            shared,
            item,
        };
        let list = vec![
            kept(false, Some("o/web"), RepoItem::Plugin { id: "on@m".into() }),
            kept(
                false,
                Some("o/web"),
                RepoItem::Plugin { id: "off@m".into() },
            ),
            kept(
                false,
                Some("o/web"),
                RepoItem::Skill {
                    entry: entry("kept"),
                },
            ),
            kept(
                false,
                Some("o/web"),
                RepoItem::Skill {
                    entry: entry("gone"),
                },
            ),
            kept(false, Some("o/web"), RepoItem::Server { name: "gh".into() }),
            // Shared ones go with git; one without an origin has nowhere to go.
            kept(true, Some("o/web"), RepoItem::Plugin { id: "on@m".into() }),
            kept(false, None, RepoItem::Plugin { id: "on@m".into() }),
        ];
        let key = dir.to_string_lossy().to_string();
        let state = json!({ "projects": { key: { "mcpServers": { "gh": { "type": "http" } } } } });
        let got = repo_mirrors(&list, &state, true);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].repo, "o/web");
        assert_eq!(got[0].plugins, vec!["on@m"]);
        assert_eq!(got[0].skills, vec![entry("kept")]);
        assert!(got[0].servers.contains_key("gh"));
        // Over plain http the MCP servers stay here.
        assert!(repo_mirrors(&list, &state, false)[0].servers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_snapshot_travels_as_a_mirror_message() {
        let s = Snapshot {
            plugins: vec!["a@m".into()],
            ..Default::default()
        };
        let msg = s.message();
        assert_eq!(msg["type"], "mirror");
        assert!(!s.is_empty());
        assert!(Snapshot::default().is_empty());
    }

    #[test]
    fn debug_names_the_servers_and_sign_ins_but_never_shows_them() {
        let server = json!({ "env": { "API_KEY": "s3cret-key" } });
        let s = Snapshot {
            servers: BTreeMap::from([("github".to_string(), server.clone())]),
            oauth: Map::from_iter([(
                "github|abc".to_string(),
                json!({ "accessToken": "s3cret-tok" }),
            )]),
            repos: vec![RepoMirror {
                repo: "acme/app".into(),
                servers: BTreeMap::from([("db".to_string(), server)]),
                ..Default::default()
            }],
            ..Default::default()
        };
        let shown = format!("{s:?}");
        assert!(
            shown.contains("github") && shown.contains("\"db\""),
            "{shown}"
        );
        assert!(!shown.contains("s3cret"), "{shown}");
    }
}
