//! Every row of the Settings dialog, once: its name, its hint, where it sits
//! and the words that find it. The dialog draws its rows and its search from
//! this list, and the palette's `:` scope lists the same rows under the same
//! names, so the two can't disagree.

use crate::agent::AgentKind;

/// Where a section sits in the list: Arakun's own settings, then the agents'.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Arakun,
    Agents,
}

impl Group {
    pub const ALL: [Group; 2] = [Group::Arakun, Group::Agents];

    pub fn title(self) -> &'static str {
        match self {
            Group::Arakun => "Arakun",
            Group::Agents => "Agents",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    General,
    Appearance,
    Project,
    Branches,
    Privacy,
    Account,
    #[cfg(feature = "coderlabs")]
    CoderSpace,
    Shortcuts,
    Claude,
    Codex,
    Droid,
    OpenCode,
    Devin,
    Cursor,
    Antigravity,
}

impl Section {
    /// In the order the list shows them, so ↑ and ↓ move down the list.
    pub const ALL: &[Section] = &[
        Section::General,
        Section::Appearance,
        Section::Project,
        Section::Branches,
        Section::Privacy,
        Section::Account,
        #[cfg(feature = "coderlabs")]
        Section::CoderSpace,
        Section::Shortcuts,
        Section::Claude,
        Section::Codex,
        Section::Droid,
        Section::OpenCode,
        Section::Devin,
        Section::Cursor,
        Section::Antigravity,
    ];

    pub fn group(self) -> Group {
        if self.agent().is_some() {
            Group::Agents
        } else {
            Group::Arakun
        }
    }

    /// The agent a section is about; the list shows its logo.
    pub fn agent(self) -> Option<AgentKind> {
        match self {
            Section::Claude => Some(AgentKind::Claude),
            Section::Codex => Some(AgentKind::Codex),
            Section::Droid => Some(AgentKind::Droid),
            Section::OpenCode => Some(AgentKind::OpenCode),
            Section::Devin => Some(AgentKind::Devin),
            Section::Cursor => Some(AgentKind::Cursor),
            Section::Antigravity => Some(AgentKind::Antigravity),
            _ => None,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Appearance => "Appearance",
            Section::Project => "Project",
            Section::Branches => "Branches",
            Section::Privacy => "Privacy",
            Section::Account => "Account",
            #[cfg(feature = "coderlabs")]
            Section::CoderSpace => "CoderSpace",
            Section::Shortcuts => "Keyboard shortcuts",
            Section::Claude => "Claude Code",
            Section::Codex => "Codex",
            Section::Droid => "Droid",
            Section::OpenCode => "OpenCode",
            Section::Devin => "Devin",
            Section::Cursor => "Cursor",
            Section::Antigravity => "Antigravity",
        }
    }

    /// The list's icon for a section that is not an agent's.
    pub fn icon(self) -> &'static str {
        match self {
            Section::General => "settings01",
            Section::Appearance => "paint-board",
            Section::Project => "folder01",
            Section::Branches => "git-branch",
            Section::Privacy => "shield-alert",
            Section::Account => "user",
            #[cfg(feature = "coderlabs")]
            Section::CoderSpace => "check-list",
            Section::Shortcuts => "keyboard",
            Section::Claude
            | Section::Codex
            | Section::Droid
            | Section::OpenCode
            | Section::Devin
            | Section::Cursor
            | Section::Antigravity => "bot",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Section::General => "How Arakun behaves when nothing is going on.",
            Section::Appearance => {
                "Tabs and sessions, and zoom. Colors, shape and the sidebar are edited beside the app, not in this dialog."
            }
            Section::Project => {
                "The open project's commands, base branch, sandbox, saved secrets and what its agents get, on this computer."
            }
            Section::Branches => {
                "A new environment starts on a random branch name; an agent can rename it after the task. A merged pull request is archived."
            }
            Section::Privacy => {
                "What shows on screen, what agents may do in the browser, and what Arakun sends to its PostHog under a random id."
            }
            Section::Account => {
                "An optional sign-in with GitHub or an email address, for what Arakun is still trying out with some people, such as Teams. Agents never need it."
            }
            #[cfg(feature = "coderlabs")]
            Section::CoderSpace => "Your pending tasks, inside Arakun.",
            Section::Shortcuts => "Everything you can do without the mouse.",
            Section::Claude => "Claude Code's sign-in and what your plan has left.",
            Section::Codex => "Codex's sign-in: your ChatGPT plan or an OpenAI API key.",
            Section::Droid => {
                "Your Factory plan, what Droid loads in the visible environment, and its questions read aloud."
            }
            Section::OpenCode => "The providers OpenCode signs in to, OpenCode Go among them.",
            Section::Devin => "Devin's sign-in: your Devin account or a Windsurf API key.",
            Section::Cursor => "Cursor's sign-in: your Cursor account or a Cursor API key.",
            Section::Antigravity => {
                "Antigravity's sign-in: your Google account or a Gemini API key."
            }
        }
    }

    /// Words for the parts of a section that are not rows of the registry,
    /// such as an agent's sign-in. Rows carry their own.
    pub fn keywords(self) -> &'static str {
        match self {
            Section::Account => "github sign in sign out login logout teams pilot device code",
            #[cfg(feature = "coderlabs")]
            Section::CoderSpace => "tasks sign in sign out email",
            Section::Shortcuts => "keys keyboard bindings hotkeys",
            Section::Claude => {
                "claude code anthropic plan usage limits weekly monthly window sign in logout"
            }
            Section::Codex => "codex openai chatgpt gpt api key sign in logout",
            Section::Droid => {
                "factory account plan usage api key credits skills mcp servers extensions plugins"
            }
            Section::OpenCode => "opencode go zen provider api key models sign in logout",
            Section::Devin => "devin cognition windsurf api key sign in logout",
            Section::Cursor => "cursor cursor-agent api key sign in logout",
            Section::Antigravity => "antigravity agy google gemini api key sign in",
            _ => "",
        }
    }

    /// Whether the search `query` (lower case, trimmed) names the section
    /// itself, which shows all of it. Not its description: that mentions
    /// rows, and a search for a row should leave that row alone.
    pub fn matches_itself(self, query: &str) -> bool {
        let text = format!("{} {}", self.title(), self.keywords()).to_lowercase();
        words_in(query, &text)
    }

    /// Whether the search leaves this section in the list: the section
    /// itself or one of its rows matches.
    pub fn matches(self, query: &str) -> bool {
        query.is_empty()
            || self.matches_itself(query)
            || ALL
                .iter()
                .any(|e| e.section == self && e.available() && e.matches(query))
    }
}

/// One row of the dialog. The variants are named after what the row sets.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Id {
    Arcade,
    Island,
    Background,
    SidebarViews,
    UsageCard,
    AutoUpdate,
    Nightly,
    Version,
    LinksInArakun,
    TabsPreset,
    SessionsIn,
    BarShows,
    StateMark,
    GroupByProject,
    SortSessions,
    QuietProjects,
    LongTitles,
    SidebarWidth,
    Colors,
    Shape,
    SidebarLook,
    Zoom,
    ReduceMotion,
    SetupScript,
    CheckCommand,
    BaseBranch,
    PassEnv,
    Sandbox,
    SavedSecrets,
    NameBranches,
    NamingModel,
    ArchiveAfterMerge,
    RemoveOldEnvironments,
    RemoveArchivedSessions,
    HideSecrets,
    Redacted,
    DrivePages,
    CrashReports,
    UsageCounts,
    Feedback,
    ReadAloud,
    VoiceTone,
    VoiceKey,
    VoiceName,
    VoiceTest,
}

/// A setting that is on or off, read and written where it is kept. What else
/// has to follow (the island shown, windows redrawn) is the shell's part:
/// `Shell::after_setting`.
#[derive(Clone, Copy)]
pub struct Switch {
    pub get: fn() -> bool,
    pub set: fn(bool),
}

pub struct Entry {
    pub id: Id,
    /// The element id of its row, and the palette row's id after `setting:`.
    pub key: &'static str,
    pub label: &'static str,
    /// The hint under the label. A few rows say more, or less, as things
    /// change; this is what the search and the palette read.
    pub hint: &'static str,
    pub section: Section,
    /// The caption of the card it is in.
    pub card: &'static str,
    /// Words the search matches besides the label, hint and card.
    pub keywords: &'static str,
    /// Set for a plain switch, which the palette turns on and off in place.
    /// Other rows open Settings on them.
    pub switch: Option<Switch>,
}

impl Entry {
    /// "Settings › General › Dynamic Island": where the row is.
    pub fn path(&self) -> String {
        format!("Settings › {} › {}", self.section.title(), self.card)
    }

    pub fn matches(&self, query: &str) -> bool {
        let text = format!(
            "{} {} {} {}",
            self.label, self.hint, self.card, self.keywords
        )
        .to_lowercase();
        words_in(query, &text)
    }

    /// Whether this build has the row at all.
    pub fn available(&self) -> bool {
        match self.id {
            Id::Background => cfg!(target_os = "linux"),
            // Windows has no agent sandbox; the row only says so.
            _ => true,
        }
    }

    pub fn on(&self) -> Option<bool> {
        self.switch.map(|s| (s.get)())
    }
}

fn words_in(query: &str, text: &str) -> bool {
    query.split_whitespace().all(|w| text.contains(w))
}

pub fn entry(id: Id) -> &'static Entry {
    ALL.iter()
        .find(|e| e.id == id)
        .expect("every Id has an entry")
}

/// Turns a switch on or off where it is kept. `None` for a row that isn't one.
pub fn set(id: Id, on: bool) -> Option<()> {
    (entry(id).switch?.set)(on);
    Some(())
}

#[cfg(target_os = "linux")]
fn background_on() -> bool {
    crate::background::enabled()
}
#[cfg(not(target_os = "linux"))]
fn background_on() -> bool {
    false
}
#[cfg(target_os = "linux")]
fn set_background(on: bool) {
    crate::background::set_enabled(on);
}
#[cfg(not(target_os = "linux"))]
fn set_background(_: bool) {}

fn read_aloud() -> bool {
    crate::voice::settings().enabled
}

fn set_read_aloud(on: bool) {
    let mut s = crate::voice::settings();
    s.enabled = on;
    crate::voice::store(&s);
    if !on {
        crate::voice::stop();
    }
}

use Section as S;

const fn sw(get: fn() -> bool, set: fn(bool)) -> Option<Switch> {
    Some(Switch { get, set })
}

/// In the order the dialog shows them.
pub static ALL: &[Entry] = &[
    // General
    Entry {
        id: Id::Arcade,
        key: "set-arcade",
        label: "Raccoon animation",
        hint: "Follows the cursor and acts out what agents do. Off, it holds still.",
        section: S::General,
        card: "Raccoon",
        keywords: "mascot animated motion arcade",
        switch: sw(crate::mascot::chosen, crate::mascot::set_animated),
    },
    Entry {
        id: Id::Island,
        key: "set-island",
        label: "Dynamic Island",
        hint: "What every agent is doing, at the top of the screen, and a card when one finishes, needs you or fails. On Linux it needs Wayland.",
        section: S::General,
        card: "Dynamic Island",
        keywords: "notch top bar overlay agents alerts notices wake word voice microphone",
        switch: sw(crate::island::enabled, crate::island::set_enabled),
    },
    Entry {
        id: Id::Background,
        key: "set-background",
        label: "Keep running in the background",
        hint: "Closing the window hides it; agents, terminals and the island go on. Run arakun to bring it back, arakun quit to quit.",
        section: S::General,
        card: "Window",
        keywords: "close hide tray quit",
        switch: sw(background_on, set_background),
    },
    Entry {
        id: Id::SidebarViews,
        key: "set-sidebar-view",
        label: "Views",
        hint: "The extra views the sidebar shows. The Marketplace has the same switches, with what each view does.",
        section: S::General,
        card: "Sidebar",
        keywords: "sidebar extensions rail github notes memory usage cloudflare posthog linear doop isometric phone servers automations",
        switch: None,
    },
    Entry {
        id: Id::UsageCard,
        key: "set-rail-usage",
        label: "Usage card",
        hint: "The plans the card at the bottom of the sidebar shows; OpenCode's once a Go key is connected. The Usage tab always shows them all.",
        section: S::General,
        card: "Sidebar",
        keywords: "plans limits",
        switch: None,
    },
    Entry {
        id: Id::AutoUpdate,
        key: "set-auto-update",
        label: "Update automatically",
        hint: "Looks for a new release on GitHub every 6 hours and downloads it. It is installed when you quit Arakun.",
        section: S::General,
        card: "Updates",
        keywords: "updates automatic auto version release upgrade",
        switch: sw(crate::update::automatic, crate::update::set_automatic),
    },
    Entry {
        id: Id::Nightly,
        key: "set-nightly",
        label: "Nightly builds",
        hint: "Builds of main from every night, before they are released. For maintainers and testers.",
        section: S::General,
        card: "Updates",
        keywords: "channel beta testers update",
        // It asks first, so it is turned on in the dialog only.
        switch: None,
    },
    Entry {
        id: Id::Version,
        key: "set-update-check",
        label: "Check for updates",
        hint: "The version of Arakun you run, and what the last check for a new one found.",
        section: S::General,
        card: "Updates",
        keywords: "update version release restart now",
        switch: None,
    },
    Entry {
        id: Id::LinksInArakun,
        key: "set-links-in-arakun",
        label: "Open links in Arakun",
        hint: "Links from sessions, the GitHub view and pins open in a browser tab. Off, they open in your own browser.",
        section: S::General,
        card: "Links",
        keywords: "browser web system default external",
        switch: sw(
            crate::browser::links_in_arakun,
            crate::browser::set_links_in_arakun,
        ),
    },
    // Appearance
    Entry {
        id: Id::TabsPreset,
        key: "tabs-presets",
        label: "Preset",
        hint: "Start from a preset, then change what you like. Changes show in the window at once.",
        section: S::Appearance,
        card: "Tabs and sessions",
        keywords: "classic focused everything layout",
        switch: None,
    },
    Entry {
        id: Id::SessionsIn,
        key: "tabs-sessions-in",
        label: "Sessions live in",
        hint: "Where every session of every project is listed",
        section: S::Appearance,
        card: "Tabs and sessions",
        keywords: "sidebar title bar both tree",
        switch: None,
    },
    Entry {
        id: Id::BarShows,
        key: "tabs-bar-shows",
        label: "Title bar shows",
        hint: "Pinned: only what you pull up from the sidebar",
        section: S::Appearance,
        card: "Tabs and sessions",
        keywords: "tabs pinned pin this project all open",
        switch: None,
    },
    Entry {
        id: Id::StateMark,
        key: "tabs-state",
        label: "State",
        hint: "How a working, waiting or failed session shows",
        section: S::Appearance,
        card: "Tabs and sessions",
        keywords: "icon dot word",
        switch: None,
    },
    Entry {
        id: Id::GroupByProject,
        key: "tabs-group",
        label: "Group by project",
        hint: "Each project heads its own sessions",
        section: S::Appearance,
        card: "Tabs and sessions",
        keywords: "tree",
        // Some layouts have no groups; the dialog says when.
        switch: None,
    },
    Entry {
        id: Id::SortSessions,
        key: "tabs-sort",
        label: "Sort sessions",
        hint: "In the sidebar tree; Recent puts the last one that moved on top",
        section: S::Appearance,
        card: "Tabs and sessions",
        keywords: "sorting order latest newest recent alphabetical a-z",
        switch: None,
    },
    Entry {
        id: Id::QuietProjects,
        key: "tabs-quiet",
        label: "Projects with nothing running",
        hint: "In the sidebar tree",
        section: S::Appearance,
        card: "Tabs and sessions",
        keywords: "quiet idle open fold folded hidden",
        switch: None,
    },
    Entry {
        id: Id::LongTitles,
        key: "tabs-titles",
        label: "Long titles",
        hint: "In the sidebar; tabs always cut",
        section: S::Appearance,
        card: "Tabs and sessions",
        keywords: "cut two lines",
        switch: None,
    },
    Entry {
        id: Id::SidebarWidth,
        key: "tabs-sidebar-w",
        label: "Sidebar width",
        hint: "The tree needs about 240 to show a title and its state",
        section: S::Appearance,
        card: "Tabs and sessions",
        keywords: "wide narrow",
        switch: None,
    },
    Entry {
        id: Id::Colors,
        key: "set-customize-colors",
        label: "Colors",
        hint: "The color scheme, its accent and grays, edited in the Customize panel.",
        section: S::Appearance,
        card: "Customize",
        keywords: "theme palette color scheme accent tint night graphite midnight forest rose diff colorblind high contrast grays gray mode dark light design system",
        switch: None,
    },
    Entry {
        id: Id::Shape,
        key: "set-customize-shape",
        label: "Shape",
        hint: "Corners, spacing, text size and glass, edited in the Customize panel.",
        section: S::Appearance,
        card: "Customize",
        keywords: "corners radius rounded round sharp spacing gap padding compact roomy density window glass blur transparent translucent text size font scale",
        switch: None,
    },
    Entry {
        id: Id::SidebarLook,
        key: "set-customize-sidebar",
        label: "Sidebar",
        hint: "Which views and blocks the sidebar shows, and in what order, edited in the Customize panel.",
        section: S::Appearance,
        card: "Customize",
        keywords: "rail blocks order collapse icons",
        switch: None,
    },
    Entry {
        id: Id::Zoom,
        key: "set-zoom",
        label: "Zoom",
        hint: "Makes the whole interface bigger or smaller, like a browser's zoom. Ctrl+= and Ctrl+- do it too, Ctrl+0 goes back to 100%.",
        section: S::Appearance,
        card: "Window",
        keywords: "bigger smaller scale",
        switch: None,
    },
    Entry {
        id: Id::ReduceMotion,
        key: "set-reduce-motion",
        label: "Reduce motion",
        hint: "Holds the mascot, spinners and the island's working dots still, and shows menus and dialogs without sliding them in. System follows your computer's setting.",
        section: S::Appearance,
        card: "Window",
        keywords: "animation accessibility still",
        switch: None,
    },
    // Project
    Entry {
        id: Id::SetupScript,
        key: "set-project-setup",
        label: "Setup script",
        hint: "Runs in a terminal of every new environment of this project on this computer, right after it is created. Not on servers. Empty turns it off.",
        section: S::Project,
        card: "This project",
        keywords: "install new environment worktree",
        switch: None,
    },
    Entry {
        id: Id::CheckCommand,
        key: "set-project-check",
        label: "Check command",
        hint: "Runs in the environment after every agent turn that edits files. When it fails, Arakun sends the output back to the agent, up to 3 times in a row. Agents can run it themselves with arakun check. Empty turns it off.",
        section: S::Project,
        card: "This project",
        keywords: "test lint build verify",
        switch: None,
    },
    Entry {
        id: Id::BaseBranch,
        key: "set-project-base",
        label: "Base branch",
        hint: "New environments of this project on this computer start from this branch as it is on origin, fetched first, or from the local branch when origin has none. Empty uses origin's default branch, usually main.",
        section: S::Project,
        card: "This project",
        keywords: "start from origin main default",
        switch: None,
    },
    Entry {
        id: Id::PassEnv,
        key: "set-project-pass-env",
        label: "Pass to agents",
        hint: "Agents don't get Arakun's own variables that look like secrets, such as GH_TOKEN or a cloud key, sandboxed or not. Their own provider keys stay. Name the variables this project's agents need, separated by spaces. Terminals keep them all. Applies to sessions started afterwards.",
        section: S::Project,
        card: "This project",
        keywords: "secrets variables env token gh_token keys",
        switch: None,
    },
    Entry {
        id: Id::Sandbox,
        key: "set-project-sandbox",
        label: "Sandbox agents",
        hint: "Agents of this project run in a sandbox. They can write only the environment, its Git folder, build caches and temporary folders, and can't read other tools' logins or files that look like secrets.",
        section: S::Project,
        card: "This project",
        keywords: "seatbelt bubblewrap isolate security",
        // It belongs to the open project, which the dialog knows.
        switch: None,
    },
    Entry {
        id: Id::SavedSecrets,
        key: "set-project-secrets",
        label: "Saved secrets",
        hint: "Secrets an agent asked for and you saved for this project, by name, with Rename, Replace and Remove, or read from a password manager such as 1Password.",
        section: S::Project,
        card: "Saved secrets",
        keywords: "api key password keyring rename replace remove variable 1password op password manager reference",
        switch: None,
    },
    // Branches
    Entry {
        id: Id::NameBranches,
        key: "set-namer",
        label: "Name new branches",
        hint: "After the first prompt, an agent names the branch after the task and Arakun renames it. The folder keeps its name.",
        section: S::Branches,
        card: "Naming",
        keywords: "branch name naming environment worktree rename agent claude droid",
        switch: None,
    },
    Entry {
        id: Id::NamingModel,
        key: "set-namer-model",
        label: "Naming model",
        hint: "A small, fast model, as the CLI names it: haiku, or a full ID. Empty uses the CLI's default.",
        section: S::Branches,
        card: "Naming",
        keywords: "model haiku branch",
        switch: None,
    },
    Entry {
        id: Id::ArchiveAfterMerge,
        key: "set-settle-after-merge",
        label: "Archive after merge",
        hint: "Once an environment's pull request is merged: its open review threads are resolved on GitHub, its GitHub notifications are marked read, and the environment's sessions stop asking for you and are archived. The GitHub tab marks read the GitHub notifications of every merged pull request.",
        section: S::Branches,
        card: "After merge",
        keywords: "merge merged archive settle resolve threads review pull request",
        switch: sw(crate::settle::on, crate::settle::set),
    },
    Entry {
        id: Id::RemoveOldEnvironments,
        key: "set-env-cleanup",
        label: "Remove old environments",
        hint: "Removes an environment's folder once its pull request is merged or closed and nobody opened it for this long. Environments with uncommitted files, commits that aren't in the pull request or a working agent stay. The branch is kept.",
        section: S::Branches,
        card: "After merge",
        keywords: "cleanup clean up remove delete old stale environments",
        switch: None,
    },
    Entry {
        id: Id::RemoveArchivedSessions,
        key: "set-session-cleanup",
        label: "Remove archived sessions after",
        hint: "Removes an archived session, with its transcript and pasted images, once this long has passed since it was archived or last used. A session that is working, waiting for you, or open in a tab stays. Off keeps sessions until you remove them.",
        section: S::Branches,
        card: "After merge",
        keywords: "cleanup clean up remove delete old archived sessions transcripts disk days",
        switch: None,
    },
    // Privacy
    Entry {
        id: Id::HideSecrets,
        key: "set-hide-secrets",
        label: "Hide secrets in sessions",
        hint: "Keys, tokens and passwords an agent prints show as stars until you click them, and copying one asks first. Redacted mode hides them too, and doesn't show them on a click.",
        section: S::Privacy,
        card: "On screen",
        keywords: "secret keys tokens mask reveal",
        switch: sw(
            crate::redact::hides_secrets,
            crate::redact::set_hides_secrets,
        ),
    },
    Entry {
        id: Id::Redacted,
        key: "set-redacted",
        label: "Redacted mode",
        hint: "For streams, demos and screen sharing: hides account emails, keys and tokens, pairing codes, addresses and your user name, in the views, the terminals and the agents' replies. Ctrl+Shift+H turns it on and off.",
        section: S::Privacy,
        card: "On screen",
        keywords: "redact hide streamer stream demo screen share sharing incognito",
        switch: sw(crate::redact::on, crate::redact::set),
    },
    Entry {
        id: Id::DrivePages,
        key: "set-drive-pages",
        label: "Agents drive any web page",
        hint: "Click, type and run scripts in any tab, with your signed-in accounts. Off, they drive local pages and only read the rest.",
        section: S::Privacy,
        card: "Browser",
        keywords: "browser web page drive click type scripts security accounts",
        switch: sw(crate::page_drive::any_page, crate::page_drive::set_any_page),
    },
    Entry {
        id: Id::CrashReports,
        key: "set-crash-reports",
        label: "Crash reports",
        hint: "Crashes, with their stack trace, and the kind of error when an agent stops. Paths, links, emails and key-like values are removed from a crash's message; its other words are sent.",
        section: S::Privacy,
        card: "Sent to Arakun",
        keywords: "telemetry crash posthog data",
        // An environment variable can decide it; the dialog says which.
        switch: None,
    },
    Entry {
        id: Id::UsageCounts,
        key: "set-usage-counts",
        label: "Usage counts",
        hint: "Counts such as sessions started, with the agent's name. Never prompts, replies, files or URLs.",
        section: S::Privacy,
        card: "Sent to Arakun",
        keywords: "telemetry analytics posthog data",
        switch: None,
    },
    Entry {
        id: Id::Feedback,
        key: "set-feedback",
        label: "Feedback",
        hint: "A bug, an idea or anything else, sent to the Arakun team when you press Send.",
        section: S::Privacy,
        card: "Sent to Arakun",
        keywords: "bug report idea send",
        switch: None,
    },
    // Droid
    Entry {
        id: Id::ReadAloud,
        key: "set-voice",
        label: "Read questions aloud",
        hint: "When Droid asks you something, Arakun says the question and its options, even with the session on screen. It stops once you answer.",
        section: S::Droid,
        card: "Read aloud",
        keywords: "voice speech speak tts audio sound elevenlabs",
        switch: sw(read_aloud, set_read_aloud),
    },
    Entry {
        id: Id::VoiceTone,
        key: "set-voice-tone",
        label: "Tone",
        hint: "How much the voice acts out what it reads. Calm sounds the steadiest.",
        section: S::Droid,
        card: "Read aloud",
        keywords: "voice calm",
        switch: None,
    },
    Entry {
        id: Id::VoiceKey,
        key: "set-voice-key",
        label: "ElevenLabs API key",
        hint: "From elevenlabs.io › Developers › API Keys. Arakun keeps it in the system keyring.",
        section: S::Droid,
        card: "ElevenLabs",
        keywords: "voice speech",
        switch: None,
    },
    Entry {
        id: Id::VoiceName,
        key: "set-voice-id",
        label: "Voice ID",
        hint: "Add a voice from the Voice Library to My Voices, then copy its ID.",
        section: S::Droid,
        card: "ElevenLabs",
        keywords: "speech library",
        switch: None,
    },
    Entry {
        id: Id::VoiceTest,
        key: "set-voice-test",
        label: "Test voice",
        hint: "Says a short sample with this key and voice.",
        section: S::Droid,
        card: "ElevenLabs",
        keywords: "speech sample",
        switch: None,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_id_has_one_entry_with_its_own_key() {
        let mut keys = std::collections::HashSet::new();
        for e in ALL {
            assert_eq!(ALL.iter().filter(|x| x.id == e.id).count(), 1, "{:?}", e.id);
            assert!(keys.insert(e.key), "{} twice", e.key);
            assert!(!e.card.is_empty(), "{:?} has no card", e.id);
        }
        // `entry` finds each one.
        assert_eq!(entry(Id::VoiceTest).id, Id::VoiceTest);
    }

    /// The dialog draws cards in registry order; a section's entries in one
    /// run each keep that order readable.
    #[test]
    fn a_section_s_rows_are_together() {
        let mut seen: Vec<Section> = Vec::new();
        for e in ALL {
            if seen.last() != Some(&e.section) {
                assert!(!seen.contains(&e.section), "{:?} split", e.section);
                seen.push(e.section);
            }
        }
    }

    #[test]
    fn paths_name_real_places() {
        assert_eq!(
            entry(Id::Island).path(),
            "Settings › General › Dynamic Island"
        );
        assert_eq!(entry(Id::Arcade).label, "Raccoon animation");
        assert_eq!(entry(Id::DrivePages).section, Section::Privacy);
        assert_eq!(entry(Id::ReadAloud).section, Section::Droid);
    }

    #[test]
    fn search_finds_settings_where_they_moved() {
        let found = |q: &str| -> Vec<Section> {
            Section::ALL
                .iter()
                .copied()
                .filter(|s| s.matches(q))
                .collect()
        };
        assert!(found("open links").contains(&Section::General));
        assert!(found("external browser").contains(&Section::General));
        assert!(found("branch").contains(&Section::Branches));
        assert!(found("haiku").contains(&Section::Branches));
        assert!(found("archive after merge").contains(&Section::Branches));
        assert!(found("settle threads").contains(&Section::Branches));
        assert!(found("crash").contains(&Section::Privacy));
        assert!(!found("crash").contains(&Section::General));
        assert!(found("update").contains(&Section::General));
        assert!(found("automatic updates").contains(&Section::General));
        assert!(found("redacted").contains(&Section::Privacy));
        assert!(found("stream").contains(&Section::Privacy));
        assert!(found("secrets").contains(&Section::Privacy));
        assert!(found("drive").contains(&Section::Privacy));
        assert!(!found("drive any web page").contains(&Section::General));
        assert!(found("elevenlabs").contains(&Section::Droid));
        assert!(found("factory").contains(&Section::Droid));
        assert!(found("github").contains(&Section::Account));
        assert!(found("github").contains(&Section::General));
        assert!(found("teams").contains(&Section::Account));
        assert!(found("zoom").contains(&Section::Appearance));
        assert!(found("glass").contains(&Section::Appearance));
        assert!(found("check command").contains(&Section::Project));
        assert!(found("setup script").contains(&Section::Project));
        assert!(found("saved secrets").contains(&Section::Project));
        assert!(found("gh_token").contains(&Section::Project));
        assert!(found("replace").contains(&Section::Project));
        assert!(found("island").contains(&Section::General));
        assert_eq!(found("").len(), Section::ALL.len());
        assert!(found("zzz-nothing").is_empty());
    }

    /// Search narrows a section to its rows: "island" leaves the island's
    /// card in General, not the updates.
    #[test]
    fn search_filters_rows() {
        let rows: Vec<Id> = ALL
            .iter()
            .filter(|e| e.matches("island"))
            .map(|e| e.id)
            .collect();
        assert!(rows.contains(&Id::Island));
        assert!(!rows.contains(&Id::AutoUpdate));
        assert!(!Section::General.matches_itself("island"));
        assert!(!Section::Appearance.matches_itself("zoom"));
    }
}
