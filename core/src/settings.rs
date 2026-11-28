//! The user's settings, in one `settings.json` next to the state file, one
//! section per Settings page. Every field has a default, so a file from an
//! older Arakun, or one edited by hand that leaves things out, still loads.
//!
//! Before this file, each setting had a file of its own, with three ways of
//! saying on or off. The first load moves what they hold in here and removes
//! them (see [`migrate`]).
//!
//! Reads come from memory. The file is read again when it changed on disk,
//! so `arakun update --channel` in a terminal reaches a running window. A
//! change reads the file first and writes back the whole of it, so it keeps
//! what another process wrote.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::agent::AgentKind;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub general: General,
    pub privacy: Privacy,
    pub branches: Branches,
}

/// Settings › General.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Where web links open; `None` until picked, and then the build decides
    /// (see `browser::links_in_arakun`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_links: Option<OpenLinks>,
    /// Agents may click, type and run scripts on any page, not only local ones.
    pub agents_drive_any_page: bool,
    /// The app looks for updates and downloads them by itself.
    pub automatic_updates: bool,
    /// The builds the updater installs; `None` until picked, and then the
    /// channel this build came from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_channel: Option<Channel>,
    /// Desktop notifications when an agent finishes, needs the user or fails
    /// and the island can't show it.
    pub notifications: bool,
    /// Agents are told what the project's memory holds, and sessions are
    /// recorded in it.
    pub memory: bool,
    /// Agents Arakun starts are told about the `arakun` command in their
    /// system prompt.
    pub agent_notes: bool,
}

impl Default for General {
    fn default() -> Self {
        General {
            open_links: None,
            agents_drive_any_page: false,
            automatic_updates: true,
            update_channel: None,
            notifications: true,
            memory: true,
            agent_notes: true,
        }
    }
}

/// Settings › Privacy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Privacy {
    /// Redacted mode: the window hides what identifies the user.
    pub redacted: bool,
    /// Sessions show secrets as stars until clicked.
    pub hide_secrets: bool,
}

impl Default for Privacy {
    fn default() -> Self {
        Privacy {
            redacted: false,
            hide_secrets: true,
        }
    }
}

/// Settings › Branches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Branches {
    /// Who names a new environment's branch after its first prompt.
    pub namer: Namer,
    /// Settle an environment once its pull request is merged (`settle`).
    pub archive_after_merge: bool,
    /// Remove old environments by themselves (`cleanup`).
    pub remove_old_environments: Cleanup,
    /// Days an archived session is kept before it and its files go; `None`
    /// keeps it until removed by hand. The app counts from when it was
    /// archived or last used; `arakun server` from when a session it lists
    /// was last used.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "keep_days"
    )]
    pub remove_archived_sessions: Option<u16>,
}

impl Default for Branches {
    fn default() -> Self {
        Branches {
            namer: Namer::default(),
            archive_after_merge: true,
            remove_old_environments: Cleanup::Unset,
            remove_archived_sessions: None,
        }
    }
}

/// `remove_archived_sessions` from the file. Anything but a number of days
/// (`"off"`, 0, a hand edit gone wrong) keeps sessions, rather than the whole
/// file set aside.
fn keep_days<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u16>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n
            .as_u64()
            .filter(|&d| d > 0)
            .map(|d| d.min(u64::from(u16::MAX)) as u16),
        serde_json::Value::String(s) => s.trim().parse::<u16>().ok().filter(|&d| d > 0),
        _ => None,
    })
}

/// Where web links open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OpenLinks {
    /// In an Arakun browser tab.
    Arakun,
    /// In the desktop's browser.
    System,
}

/// Which builds the updater installs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Nightly,
}

impl Channel {
    pub fn parse(s: &str) -> Option<Channel> {
        match s.trim() {
            "stable" => Some(Channel::Stable),
            "nightly" => Some(Channel::Nightly),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Nightly => "nightly",
        }
    }
}

/// Whether old environments go by themselves. In the file: `"suggested"`,
/// `"off"` or a number of days; left out while unset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cleanup {
    /// Never picked, nor suggested yet: off.
    #[default]
    Unset,
    /// Off, after the suggestion was shown and left alone.
    Suggested,
    Off,
    Days(u8),
}

impl Cleanup {
    /// Days an environment must sit unused; `None` when off.
    pub fn days(self) -> Option<u8> {
        match self {
            Cleanup::Days(d) if d > 0 => Some(d),
            _ => None,
        }
    }

    /// The choice as the old `env-cleanup` file wrote it.
    fn parse(text: &str) -> Cleanup {
        match text.trim() {
            "off" => Cleanup::Off,
            "suggested" => Cleanup::Suggested,
            n => match n.parse::<u8>() {
                Ok(0) => Cleanup::Off,
                Ok(d) => Cleanup::Days(d),
                Err(_) => Cleanup::Unset,
            },
        }
    }
}

impl Serialize for Cleanup {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Cleanup::Unset => s.serialize_none(),
            Cleanup::Suggested => s.serialize_str("suggested"),
            Cleanup::Off | Cleanup::Days(0) => s.serialize_str("off"),
            Cleanup::Days(d) => s.serialize_u8(*d),
        }
    }
}

impl<'de> Deserialize<'de> for Cleanup {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Anything else is a hand edit gone wrong: off, rather than the whole
        // file set aside.
        Ok(match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(s) => Cleanup::parse(&s),
            serde_json::Value::Number(n) => match n.as_u64() {
                Some(0) => Cleanup::Off,
                Some(d) => Cleanup::Days(d.min(u8::MAX as u64) as u8),
                None => Cleanup::Unset,
            },
            _ => Cleanup::Unset,
        })
    }
}

/// Who names a new environment's branch after its first prompt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NamerChoice {
    /// The agent the first prompt went to, when it is one of [`NAMER_AGENTS`],
    /// so the prompt reaches no service the user didn't pick. Other agents
    /// leave the branch alone.
    #[default]
    Session,
    Off,
    Claude,
    Droid,
}

impl NamerChoice {
    pub const ALL: [NamerChoice; 4] = [
        NamerChoice::Session,
        NamerChoice::Off,
        NamerChoice::Claude,
        NamerChoice::Droid,
    ];

    pub fn label(self) -> &'static str {
        match self {
            NamerChoice::Session => "Session's agent",
            NamerChoice::Off => "Off",
            NamerChoice::Claude => AgentKind::Claude.label(),
            NamerChoice::Droid => AgentKind::Droid.label(),
        }
    }

    /// The agent picked for every environment, whatever its sessions use.
    pub fn agent(self) -> Option<AgentKind> {
        match self {
            NamerChoice::Claude => Some(AgentKind::Claude),
            NamerChoice::Droid => Some(AgentKind::Droid),
            NamerChoice::Session | NamerChoice::Off => None,
        }
    }
}

/// Who names branches, and with what model.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Namer {
    pub choice: NamerChoice,
    /// The model of an agent picked in Settings; empty uses the CLI's own
    /// default. Following the session uses [`default_namer_model`].
    pub model: String,
}

impl Namer {
    /// The agent and model that name a branch whose first prompt went to
    /// `session`; `None` when nobody does.
    pub fn pick(&self, session: AgentKind) -> Option<(AgentKind, String)> {
        match self.choice {
            NamerChoice::Off => None,
            NamerChoice::Session => NAMER_AGENTS
                .contains(&session)
                .then(|| (session, default_namer_model(session).to_string())),
            NamerChoice::Claude | NamerChoice::Droid => {
                self.choice.agent().map(|a| (a, self.model.clone()))
            }
        }
    }
}

/// The agents that can name a branch: CLIs that answer one prompt, then exit,
/// without running tools. OpenCode is not one: `opencode run` may run shell
/// commands and edit files by default, and its permission config differs
/// between versions.
pub const NAMER_AGENTS: [AgentKind; 2] = [AgentKind::Claude, AgentKind::Droid];

/// A small, fast model for each CLI. Droid's model ids depend on the user's
/// plan, so it starts on the CLI's default.
pub fn default_namer_model(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "haiku",
        _ => "",
    }
}

// ---- what the environment decides ------------------------------------------------

pub use crate::stored::Saving;

/// A setting a variable decides for this run, whatever Settings holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Forced {
    /// The variable, which Settings names next to the switch.
    pub var: &'static str,
    pub on: bool,
}

/// What `var` decides, read by `means` from its trimmed value: `None` when it
/// is not set or says nothing `means` knows.
pub fn forced(var: &'static str, means: fn(&str) -> Option<bool>) -> Option<Forced> {
    let value = std::env::var(var).ok()?;
    means(value.trim()).map(|on| Forced { var, on })
}

/// The setting, unless the environment decides it.
fn decided(forced: Option<Forced>, saved: fn(&Settings) -> bool) -> bool {
    forced.map_or_else(|| read(saved), |f| f.on)
}

fn off_only(v: &str) -> Option<bool> {
    (v == "off").then_some(false)
}

/// `ARAKUN_NOTIFY=off` turns desktop notifications off for the run.
pub fn notifications_forced() -> Option<Forced> {
    forced("ARAKUN_NOTIFY", off_only)
}

pub fn notifications() -> bool {
    decided(notifications_forced(), |s| s.general.notifications)
}

pub fn set_notifications(on: bool) -> Saving {
    change(|s| s.general.notifications = on)
}

/// `ARAKUN_MEMORY=off` keeps memory out of prompts and stops recording
/// sessions in it; `on` keeps it in even without the agent notes.
pub fn memory_forced() -> Option<Forced> {
    forced("ARAKUN_MEMORY", |v| match v {
        "off" => Some(false),
        "on" => Some(true),
        _ => None,
    })
}

pub fn memory() -> bool {
    decided(memory_forced(), |s| s.general.memory)
}

pub fn set_memory(on: bool) -> Saving {
    change(|s| s.general.memory = on)
}

/// `ARAKUN_AGENT_HINT=off` leaves agents' system prompts alone; `arakun
/// server` sets it for itself.
pub fn agent_notes_forced() -> Option<Forced> {
    forced("ARAKUN_AGENT_HINT", off_only)
}

pub fn agent_notes() -> bool {
    decided(agent_notes_forced(), |s| s.general.agent_notes)
}

pub fn set_agent_notes(on: bool) -> Saving {
    change(|s| s.general.agent_notes = on)
}

// ---- loading and saving ----------------------------------------------------------

/// What was loaded, and the file it came from as it was then.
struct Cached {
    stamp: Option<Stamp>,
    settings: Settings,
}

/// When a file changed and how long it is: enough to see another process
/// wrote it.
type Stamp = (SystemTime, u64);

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

/// The settings as they are now.
pub fn get() -> Settings {
    read(Settings::clone)
}

/// `f` of the settings as they are now, without a copy.
pub fn read<R>(f: impl FnOnce(&Settings) -> R) -> R {
    let path = file();
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    f(&fresh(&mut cache, &path).settings)
}

/// Changes the settings and saves them in the background, for a click in
/// Settings. The change holds for this run even when the file can't be
/// written; that is logged, and what comes back says so for the control
/// that made the change.
pub fn change(f: impl FnOnce(&mut Settings)) -> Saving {
    let path = file();
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cached = fresh(&mut cache, &path);
    f(&mut cached.settings);
    crate::stored::write_soon_saving(&path, to_bytes(&cached.settings), "could not save settings")
}

/// `change`, saved before it returns: for a command that exits right after,
/// or a caller that must know it lasted.
pub fn change_now(f: impl FnOnce(&mut Settings)) -> std::io::Result<()> {
    let path = file();
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cached = fresh(&mut cache, &path);
    f(&mut cached.settings);
    let saved = crate::stored::write(&path, &to_bytes(&cached.settings));
    // What this write left on disk is what is in memory.
    cached.stamp = stamp(&path);
    saved
}

fn file() -> PathBuf {
    crate::paths::store(crate::paths::SETTINGS)
}

fn to_bytes(s: &Settings) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(s).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// The cached settings, read again when the file changed since.
fn fresh<'a>(cache: &'a mut Option<Cached>, path: &Path) -> &'a mut Cached {
    let now = stamp(path);
    match cache {
        Some(c) if c.stamp == now => {}
        Some(c) => {
            // Changed on disk. A file that is gone, or can't be read, keeps
            // what this run has: the next change writes it again.
            if let Some(s) = crate::stored::load(path) {
                c.settings = s;
            }
            c.stamp = now;
        }
        None => {
            let settings = load_or_migrate(path);
            *cache = Some(Cached {
                stamp: stamp(path),
                settings,
            });
        }
    }
    cache.as_mut().expect("filled above")
}

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// The settings in `path`, or, when there is no such file yet, what the old
/// files next to it held.
fn load_or_migrate(path: &Path) -> Settings {
    if std::fs::symlink_metadata(path).is_ok() {
        // One that can't be read is set aside and the defaults take over:
        // the old files are gone by now.
        return crate::stored::load(path).unwrap_or_default();
    }
    let Some(dir) = path.parent() else {
        return Settings::default();
    };
    migrate(dir, path)
}

// ---- the files from before -------------------------------------------------------

/// The files each setting had before `settings.json`, by name in the data
/// folder.
const REDACTED: &str = "redacted";
const SHOW_SECRETS: &str = "show-secrets";
const DRIVE_ANY_PAGE: &str = "drive-any-page";
const NO_AUTO_UPDATE: &str = "no-auto-update";
const OPEN_LINKS: &str = "open-links";
const UPDATE_CHANNEL: &str = "update-channel";
const SETTLE_AFTER_MERGE: &str = "settle-after-merge";
const ENV_CLEANUP: &str = "env-cleanup";
const BRANCH_NAMER: &str = "branch-namer.json";
const OLD: [&str; 9] = [
    REDACTED,
    SHOW_SECRETS,
    DRIVE_ANY_PAGE,
    NO_AUTO_UPDATE,
    OPEN_LINKS,
    UPDATE_CHANNEL,
    SETTLE_AFTER_MERGE,
    ENV_CLEANUP,
    BRANCH_NAMER,
];

/// Reads the old files in `dir` into settings. When there was one, saves them
/// to `path` and, once that worked, removes the old files, so this runs once.
fn migrate(dir: &Path, path: &Path) -> Settings {
    let old = |name: &str| dir.join(name);
    let text = |name: &str| std::fs::read_to_string(old(name)).ok();
    if !OLD.iter().any(|name| old(name).exists()) {
        return Settings::default();
    }
    let mut s = Settings::default();
    // Markers: the file being there says it.
    s.privacy.redacted = old(REDACTED).is_file();
    s.privacy.hide_secrets = !old(SHOW_SECRETS).is_file();
    s.general.agents_drive_any_page = old(DRIVE_ANY_PAGE).is_file();
    s.general.automatic_updates = !old(NO_AUTO_UPDATE).exists();
    // Words.
    s.general.open_links = text(OPEN_LINKS).and_then(|t| match t.trim() {
        "arakun" => Some(OpenLinks::Arakun),
        "system" => Some(OpenLinks::System),
        _ => None,
    });
    s.general.update_channel = text(UPDATE_CHANNEL).and_then(|t| Channel::parse(&t));
    if let Some(t) = text(SETTLE_AFTER_MERGE) {
        s.branches.archive_after_merge = t.trim() != "off";
    }
    s.branches.remove_old_environments = text(ENV_CLEANUP)
        .map(|t| Cleanup::parse(&t))
        .unwrap_or_default();
    s.branches.namer = std::fs::read(old(BRANCH_NAMER))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    match crate::stored::write(path, &to_bytes(&s)) {
        Ok(()) => {
            for name in OLD {
                match std::fs::remove_file(old(name)) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                        eprintln!("arakun: could not remove {}: {e}", old(name).display());
                    }
                    _ => {}
                }
            }
        }
        // The old files stay, and the next start tries again.
        Err(e) => eprintln!("arakun: could not save {}: {e}", path.display()),
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stored::tests::scratch;

    #[test]
    fn archived_sessions_stay_unless_a_number_of_days_is_set() {
        let read = |json: &str| -> Option<u16> {
            let s: Settings = serde_json::from_str(&format!(
                r#"{{"branches":{{"remove_archived_sessions":{json}}}}}"#
            ))
            .unwrap();
            s.branches.remove_archived_sessions
        };
        assert_eq!(read("90"), Some(90));
        assert_eq!(read("\"30\""), Some(30));
        assert_eq!(read("365"), Some(365));
        assert_eq!(read("0"), None);
        assert_eq!(read("\"off\""), None);
        assert_eq!(read("true"), None);
        assert_eq!(read("null"), None);
        assert_eq!(Settings::default().branches.remove_archived_sessions, None);
        // Off leaves nothing in the file.
        let json = serde_json::to_value(Settings::default()).unwrap();
        assert!(json["branches"].get("remove_archived_sessions").is_none());
        let mut on = Settings::default();
        on.branches.remove_archived_sessions = Some(30);
        let json = serde_json::to_value(&on).unwrap();
        assert_eq!(json["branches"]["remove_archived_sessions"], 30);
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Settings::default());
        assert!(s.privacy.hide_secrets);
        assert!(s.general.automatic_updates);
        assert!(s.branches.archive_after_merge);
        assert!(!s.privacy.redacted);
    }

    #[test]
    fn a_file_from_before_the_agent_switches_keeps_them_on() {
        let s: Settings =
            serde_json::from_str(r#"{"general": {"automatic_updates": false}}"#).unwrap();
        assert!(s.general.notifications && s.general.memory && s.general.agent_notes);
    }

    #[test]
    fn a_variable_decides_only_with_a_value_it_knows() {
        assert_eq!(off_only("off"), Some(false));
        assert_eq!(off_only("on"), None);
        assert_eq!(off_only(""), None);
        // Decided, the saved setting is not read.
        let saved: fn(&Settings) -> bool = |_| unreachable!();
        assert!(!decided(
            Some(Forced {
                var: "X",
                on: false
            }),
            saved
        ));
        assert!(decided(Some(Forced { var: "X", on: true }), saved));
    }

    #[test]
    fn a_file_with_some_fields_keeps_the_defaults_of_the_rest() {
        let s: Settings =
            serde_json::from_str(r#"{"privacy": {"redacted": true}, "unknown": 1}"#).unwrap();
        assert!(s.privacy.redacted);
        assert!(s.privacy.hide_secrets);
    }

    #[test]
    fn settings_read_back_what_they_wrote() {
        let mut s = Settings::default();
        s.general.open_links = Some(OpenLinks::System);
        s.general.update_channel = Some(Channel::Nightly);
        s.branches.namer = Namer {
            choice: NamerChoice::Claude,
            model: "haiku".into(),
        };
        for cleanup in [
            Cleanup::Unset,
            Cleanup::Suggested,
            Cleanup::Off,
            Cleanup::Days(14),
        ] {
            s.branches.remove_old_environments = cleanup;
            let back: Settings = serde_json::from_slice(&to_bytes(&s)).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn the_cleanup_choice_reads_what_a_person_might_write() {
        let read = |json: &str| -> Cleanup { serde_json::from_str(json).unwrap() };
        assert_eq!(read("0"), Cleanup::Off);
        assert_eq!(read("\"off\""), Cleanup::Off);
        assert_eq!(read("30"), Cleanup::Days(30));
        assert_eq!(read("\"7\""), Cleanup::Days(7));
        assert_eq!(read("true"), Cleanup::Unset);
        assert_eq!(read("null"), Cleanup::Unset);
        assert_eq!(Cleanup::Days(14).days(), Some(14));
        assert_eq!(Cleanup::Suggested.days(), None);
    }

    #[test]
    fn nothing_to_migrate_writes_no_file() {
        let dir = scratch("settings-fresh");
        let path = dir.join("settings.json");
        assert_eq!(load_or_migrate(&path), Settings::default());
        assert!(!path.exists());
    }

    #[test]
    fn the_old_files_move_in_once() {
        let dir = scratch("settings-migrate");
        let path = dir.join("settings.json");
        let put = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        put(REDACTED, "hide personal data on screen\n");
        put(SHOW_SECRETS, "show secrets in sessions\n");
        put(DRIVE_ANY_PAGE, "agents may drive external pages\n");
        put(NO_AUTO_UPDATE, "do not update arakun by itself\n");
        put(OPEN_LINKS, "system\n");
        put(UPDATE_CHANNEL, "nightly\n");
        put(SETTLE_AFTER_MERGE, "off\n");
        put(ENV_CLEANUP, "30\n");
        put(BRANCH_NAMER, r#"{"choice": "droid", "model": "fast"}"#);
        put("state.json", "{}");

        let s = load_or_migrate(&path);
        assert!(s.privacy.redacted);
        assert!(!s.privacy.hide_secrets);
        assert!(s.general.agents_drive_any_page);
        assert!(!s.general.automatic_updates);
        assert_eq!(s.general.open_links, Some(OpenLinks::System));
        assert_eq!(s.general.update_channel, Some(Channel::Nightly));
        assert!(!s.branches.archive_after_merge);
        assert_eq!(s.branches.remove_old_environments, Cleanup::Days(30));
        assert_eq!(s.branches.namer.choice, NamerChoice::Droid);
        assert_eq!(s.branches.namer.model, "fast");

        for name in OLD {
            assert!(!dir.join(name).exists(), "{name} is still there");
        }
        assert!(dir.join("state.json").exists());
        assert_eq!(crate::stored::load::<Settings>(&path), Some(s.clone()));
        // Once moved, the file is what counts.
        assert_eq!(load_or_migrate(&path), s);
    }

    #[test]
    fn one_old_file_keeps_the_defaults_of_the_rest() {
        let dir = scratch("settings-one-old");
        let path = dir.join("settings.json");
        std::fs::write(dir.join(SETTLE_AFTER_MERGE), "on\n").unwrap();
        let s = load_or_migrate(&path);
        assert_eq!(s, Settings::default());
        assert!(path.exists());
        assert!(!dir.join(SETTLE_AFTER_MERGE).exists());
    }

    #[test]
    fn a_change_on_disk_is_read_again() {
        let dir = scratch("settings-reread");
        let path = dir.join("settings.json");
        let mut cache = None;
        assert!(!fresh(&mut cache, &path).settings.privacy.redacted);
        std::fs::write(&path, r#"{"privacy": {"redacted": true}}"#).unwrap();
        assert!(fresh(&mut cache, &path).settings.privacy.redacted);
        // A file that is gone keeps what this run has.
        std::fs::remove_file(&path).unwrap();
        assert!(fresh(&mut cache, &path).settings.privacy.redacted);
    }
}
