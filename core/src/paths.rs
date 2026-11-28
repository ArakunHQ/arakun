//! Where Arakun keeps its files. Every module asks here, so one rule decides
//! what a second Arakun with its own state (`ARAKUN_STATE`, as `arakun dev`
//! sets it) keeps apart from the person's own.
//!
//! - [`store`]: this instance's own files: settings, transcripts, memory,
//!   history. They sit next to the state file, so `ARAKUN_STATE` moves all of
//!   them at once. Without it, that is the data folder.
//! - [`shared`]: what every instance on the machine shares: caches, installed
//!   agents and tools, the updater's download, `arakun server`'s folder, and
//!   stores whose secrets the keyring keeps under one name per machine (a test
//!   Arakun with its own file there would sign the person's own Arakun out).
//!
//! Both take only a name from the list below, so every file is in it, and
//! the storage tables in `docs/windows.md` and `docs/storage-and-env.md` are
//! rendered from it (see the tests at the bottom).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// A file or folder of one Arakun's own, for [`store`].
#[derive(Clone, Copy, Debug)]
pub struct Own(&'static str);

/// A file or folder every Arakun on the machine shares, for [`shared`].
#[derive(Clone, Copy, Debug)]
pub struct Shared(&'static str);

impl Own {
    /// The name alone, for code that tests put in a folder of their own.
    pub const fn name(self) -> &'static str {
        self.0
    }
}

impl Shared {
    /// The name alone, for code that tests put in a folder of their own.
    pub const fn name(self) -> &'static str {
        self.0
    }
}

/// One line of the list, for the docs.
#[derive(Debug)]
pub struct Entry {
    /// The name in its folder, with `/` between folders.
    pub name: &'static str,
    /// Whether it moves with `ARAKUN_STATE`.
    pub own: bool,
    pub folder: bool,
    /// The one system it exists on, if not all.
    pub only: Option<&'static str>,
    /// What it holds, for a person reading the docs.
    pub what: &'static str,
}

macro_rules! files {
    ($($kind:ident $form:ident $([$os:ident])? $id:ident = $name:literal, $what:literal;)*) => {
        $(
            #[doc = $what]
            pub const $id: files!(@ty $kind) = files!(@make $kind $name);
        )*
        /// Every file and folder, in the order the docs list them.
        pub const FILES: &[Entry] = &[$(Entry {
            name: $name,
            own: files!(@own $kind),
            folder: files!(@folder $form),
            only: files!(@only $($os)?),
            what: $what,
        }),*];
    };
    (@ty own) => { Own };
    (@ty shared) => { Shared };
    (@make own $name:literal) => { Own($name) };
    (@make shared $name:literal) => { Shared($name) };
    (@own own) => { true };
    (@own shared) => { false };
    (@folder file) => { false };
    (@folder folder) => { true };
    (@only) => { None };
    (@only $os:ident) => { Some(stringify!($os)) };
}

// `kind form [only on] NAME = "name", "what it holds";`. The text goes into
// the docs as it is: Markdown, one plain sentence part.
files! {
    own folder SESSIONS = "sessions", "Sessions and their transcripts";
    own folder LINKED = "linked", "Sessions written out as Markdown for another agent to read";
    own folder PASTES = "pastes", "Images pasted into the composer or sent from the phone";
    own folder HEADLESS = "headless", "Sessions that run without a window";
    own folder MEMORY = "memory", "Memory (or the folder `ARAKUN_MEMORY_DIR` names)";
    own file SETTINGS = "settings.json", "Your settings from Settings › General, Privacy and Branches";
    own file SIDEBAR = "sidebar.json", "The sidebar's layout";
    own file EXTENSIONS = "extensions.json", "The extension views turned on in the Marketplace";
    own file HISTORY = "history.json", "Prompts sent from the composer, for ↑ and ↓";
    own file PROMPTS = "prompts.json", "Saved prompts";
    own file NOTICES = "notices.json", "Notice history";
    own file PALETTES = "palettes.json", "Saved palettes";
    own file RULES = "rules.json", "`Ctrl+K` rules";
    own file PINS = "pins.json", "What you pinned from the palette (`Alt+P`)";
    own file TODOS = "todos.json", "Your to-dos, added from the palette";
    own file FRECENCY = "palette-frecency.json", "What the palette ranks first";
    own file VOICE = "voice.json", "Voice settings";
    own file PHONE = "phone.json", "The Phone tab's settings and pairings (the private keys are in the keyring)";
    own file SERVERS = "servers.json", "Servers the app is paired with or reaches through your account (Computers tab)";
    own file SERVERS_INBOX = "servers-inbox.json", "What each paired server listed last";
    own file GITHUB = "github.json", "GitHub tab settings";
    own folder GITHUB_DRAFTS = "github/drafts", "Review drafts (GitHub tab)";
    own file PILOT = "pilot.json", "The PR agent's cards you saw and dealt with (arakun-pilot)";
    own file SETTLED = "settled.json", "The merges archived";
    own file TELEMETRY = "telemetry.json", "Your answer about crash reports and usage counts (Settings › Privacy)";
    own file UPDATE_CHANNEL_LOCK = "update-channel.lock", "Held while the update channel switches";
    own file CRASH_LOG = "crash.log", "Every panic, kept on this machine";
    own file RUNNING = "running", "That Arakun is running: one left behind means the last run crashed";
    shared file TELEMETRY_ID = "telemetry-id", "The random install id for crash reports and usage counts";
    shared file SURVEYS = "surveys.json", "Surveys from the last fetch";
    shared file SURVEYS_SEEN = "surveys-seen.json", "The surveys you saw";
    shared folder AGENTS = "agents", "Agent adapters (npm), and each agent's log `<agent>.log` with one older copy `<agent>.old.log`";
    shared file USAGE = "usage.jsonl", "The usage log and its older copy `usage.old.jsonl` (or the file `ARAKUN_USAGE_LOG` names)";
    shared folder SEATS = "seats", "Claude seats (`arakun seat add`), one empty file per seat; their tokens are in the keyring";
    shared file CLAUDE_SEATS = "claude-seats.json", "The Claude seats' last limits";
    shared file CLAUDE_KEY_HELPER = "claude-key-helper.json", "The settings that hand Claude Code its API key helper";
    shared file OPENCODE_PLAN = "opencode-plan.json", "OpenCode's plan and limits";
    shared file DEVIN_PLAN = "devin-plan.json", "Devin's plan and limits";
    shared file CLIPROXY_KEY = "cliproxy-management-key", "The proxy the CLIProxyAPI management key is for (Usage tab); the key is in the keyring";
    shared file CLIPROXY_ADDRESS = "cliproxy-address", "A typed CLIProxyAPI address (Usage tab)";
    shared file CLIPROXY_PLAN = "cliproxy-plan.json", "The proxy's accounts' last limits (Usage tab)";
    shared file SKILLS_INSTALLED = "skills-installed.json", "The skills.sh skills installed";
    shared file REPO_INSTALLS = "repo-installs.json", "Skills and servers installed from repositories";
    shared file SIGN_INS_COPIED = "sign-ins-copied.json", "On a server: which paired device copied each MCP sign-in there, so it goes when the device does";
    shared file SECRET_REFS = "secret-refs.json", "An index of the secrets in the keyring (hashes of their handles only)";
    shared folder SECRET_FILES = "secret-files", "Secrets handed to agents as files, removed once unused";
    shared file SECRETS = "secrets.json", "Arakun's own secrets, when there is no keyring";
    shared folder SANDBOXES = "sandboxes", "Each sandboxed agent's folder while it runs";
    shared file SANDBOX_SOCKET = "sandbox/control.sock", "The socket sandboxed agents reach Arakun through";
    shared file LINEAR_SESSIONS = "linear/sessions.json", "Linear sign-ins (who signed in; the API keys are in the keyring)";
    shared file LINEAR_DELEGATION = "linear/delegation.json", "Linear: the switch for starting sessions, its mappings, how many run at once on each machine and whether pull requests are marked as the agent's (no keys)";
    shared file LINEAR_SERVERS = "linear/servers.json", "Linear: the servers that hold Linear settings";
    shared file LINEAR_RUNS = "linear/runs.json", "Linear: the sessions started from Linear (deliveries handled, runs, reports not yet sent)";
    shared folder DOOP = "doop", "Doop sign-ins (`sessions.json`; their keys and tokens are in the keyring) and canvases added by link (`pins.json`)";
    shared folder POSTHOG = "posthog", "PostHog sign-ins (`sessions.json`; the keys are in the keyring) and links (`links.json`)";
    shared folder CACHE = "cache", "Caches: Doop renders, PostHog answers, favicons, marketplaces, extensions";
    shared folder SERVER = "server", "`arakun server`: its settings, sign-in, Linear settings and runs, sessions and socket (or the folder `ARAKUN_SERVER_DIR` names)";
    shared file UPDATE = "update.json", "Updates: the version waiting";
    shared file UPDATE_LOCK = "update.lock", "Held while an update downloads";
    shared folder UPDATE_DOWNLOAD = "update-download", "The update being downloaded";
    shared file [windows] LOG = "arakun.log", "Log, when started without a terminal";
    shared file [macos] LOGIN_PATH = "login-path.txt", "The login shell's `PATH`, so a start from the Dock finds tools";
}

/// The state file: `ARAKUN_STATE`, or `state.json` in the data folder.
pub fn state_file() -> PathBuf {
    state_file_in(std::env::var_os("ARAKUN_STATE"), crate::platform::app_dir())
}

/// The folder this instance's own files are in: the state file's.
pub fn data_dir() -> PathBuf {
    data_dir_in(std::env::var_os("ARAKUN_STATE"), crate::platform::app_dir())
}

/// A file or folder of this instance's own, next to the state file.
pub fn store(file: Own) -> PathBuf {
    under(data_dir(), file.0)
}

/// A file or folder every Arakun on this machine shares, in the data folder
/// whatever `ARAKUN_STATE` says.
pub fn shared(file: Shared) -> PathBuf {
    under(crate::platform::app_dir(), file.0)
}

/// `name`'s parts one by one, so Windows gets `\` between them.
fn under(dir: PathBuf, name: &str) -> PathBuf {
    name.split('/').fold(dir, |path, part| path.join(part))
}

fn state_file_in(state: Option<OsString>, app_dir: PathBuf) -> PathBuf {
    state
        .map(PathBuf::from)
        .unwrap_or_else(|| app_dir.join("state.json"))
}

fn data_dir_in(state: Option<OsString>, app_dir: PathBuf) -> PathBuf {
    match state {
        // A bare `state.json` has the empty parent: the working directory, as
        // the state file itself is read from there.
        Some(state) => Path::new(&state)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default(),
        None => app_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_state_file_everything_is_in_the_data_folder() {
        let app = PathBuf::from("/data/arakun");
        assert_eq!(state_file_in(None, app.clone()), app.join("state.json"));
        assert_eq!(data_dir_in(None, app.clone()), app);
    }

    #[test]
    fn a_state_file_takes_the_instance_files_with_it() {
        let app = PathBuf::from("/data/arakun");
        let state = Some(OsString::from("/tmp/dev/state.json"));
        assert_eq!(
            state_file_in(state.clone(), app.clone()),
            PathBuf::from("/tmp/dev/state.json")
        );
        assert_eq!(data_dir_in(state, app), PathBuf::from("/tmp/dev"));
    }

    #[test]
    fn a_bare_state_file_keeps_them_in_the_working_directory() {
        let app = PathBuf::from("/data/arakun");
        let dir = data_dir_in(Some(OsString::from("state.json")), app.clone());
        assert_eq!(dir.join("voice.json"), PathBuf::from("voice.json"));
        let dir = data_dir_in(Some(OsString::new()), app);
        assert_eq!(dir.join("voice.json"), PathBuf::from("voice.json"));
    }

    #[test]
    fn a_name_with_folders_is_joined_part_by_part() {
        let dir = under(PathBuf::from("data"), "linear/runs.json");
        assert_eq!(dir.components().count(), 3);
        assert!(dir.ends_with(Path::new("linear").join("runs.json")));
    }

    #[test]
    fn every_name_is_listed_once() {
        let mut names: Vec<_> = FILES.iter().map(|f| f.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "a name is in the list twice");
    }

    const BEGIN: &str = "<!-- paths: generated from core/src/paths.rs -->\n";
    const END: &str = "<!-- paths: end -->\n";

    /// The two tables, for the data folder at `root` with `sep` between
    /// folders, of the files that exist on one of `systems`.
    fn tables(root: &str, sep: &str, systems: &[&str]) -> String {
        let table = |own: bool| {
            let mut out = String::from("| What | Where |\n|---|---|\n");
            for f in FILES
                .iter()
                .filter(|f| f.own == own && f.only.is_none_or(|os| systems.contains(&os)))
            {
                let slash = if f.folder { sep } else { "" };
                let name = f.name.replace('/', sep);
                let what = match f.only {
                    Some(os) if systems.len() > 1 => {
                        let os = if os == "macos" { "macOS" } else { "Linux" };
                        format!("{} ({os} only)", f.what)
                    }
                    _ => f.what.to_string(),
                };
                out += &format!("| {what} | `{root}{name}{slash}` |\n");
            }
            out
        };
        format!(
            "{BEGIN}\n### This Arakun's own\n\n\
             With `ARAKUN_STATE` set, these are in the folder of that file instead.\n\n\
             {}\n### Shared by every Arakun on this computer\n\n\
             These stay in `{root}` whatever `ARAKUN_STATE` says.\n\n\
             {}\n{END}",
            table(true),
            table(false)
        )
    }

    /// Checks the tables between the markers of `doc`, or writes them there
    /// with `ARAKUN_BLESS=1`.
    fn check_tables(doc: &str, want: String) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(doc);
        let text = std::fs::read_to_string(&path).unwrap();
        let start = text
            .find(BEGIN)
            .unwrap_or_else(|| panic!("{doc} has no paths: begin marker"));
        let end = text[start..]
            .find(END)
            .unwrap_or_else(|| panic!("{doc} has no paths: end marker"))
            + start
            + END.len();
        if text[start..end] == want {
            return;
        }
        if std::env::var_os("ARAKUN_BLESS").is_some() {
            std::fs::write(&path, format!("{}{want}{}", &text[..start], &text[end..])).unwrap();
            return;
        }
        panic!(
            "the storage tables in {doc} are not the list in core/src/paths.rs; \
             write them with `ARAKUN_BLESS=1 cargo test -p arakun-core paths`"
        );
    }

    /// Run with `ARAKUN_BLESS=1` to write the tables into the docs.
    #[test]
    fn the_windows_docs_list_every_file() {
        check_tables(
            "docs/windows.md",
            tables("%LOCALAPPDATA%\\arakun\\", "\\", &["windows"]),
        );
    }

    /// Run with `ARAKUN_BLESS=1` to write the tables into the docs.
    #[test]
    fn the_storage_docs_list_every_file() {
        check_tables(
            "docs/storage-and-env.md",
            tables("~/.local/share/arakun/", "/", &["linux", "macos"]),
        );
    }
}
