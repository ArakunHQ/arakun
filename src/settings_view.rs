//! The Settings dialog: the search and the sections on the left, grouped as
//! Arakun's own and the agents', and the chosen section on the right as
//! captioned cards of rows.

use crate::ui::Typography as _;
use std::sync::atomic::{AtomicU8, Ordering};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};

use crate::agent::AgentKind;
use crate::agent::claude_auth;
use crate::agent::plan::{self, PlanUsage};
use crate::env_name;
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, ButtonKind, icon};
use crate::voice::{self, Tone};

pub enum SettingsEvent {
    Dismissed,
    ClaudeSignIn,
    ClaudeSignOut,
    OpenCodeSignIn,
    OpenCodeGoSignIn,
    OpenCodeSignOut,
    CodexSignIn,
    CodexSignOut,
    DevinSignIn,
    DevinSignOut,
    CursorSignIn,
    CursorSignOut,
    AntigravitySignIn,
    OpenUsage,
    /// Close Settings and open the Customize panel.
    OpenCustomize,
    RailUsage(AgentKind, bool),
    Arcade(bool),
    Island(bool),
    /// The whole UI's zoom, in percent.
    Zoom(u32),
    /// How sessions and tabs show, and the preset it came from, if any.
    TabLayout(
        crate::tab_layout::TabLayout,
        Option<crate::tab_layout::Preset>,
    ),
    /// The sidebar's width, in points.
    SidebarWidth(f32),
    ReduceMotion(crate::ui::motion::Reduce),
    /// An extension view turned on or off in the sidebar.
    SidebarView(crate::sidebar::View, bool),
    /// A project's setup script (`check` false) or check command; `None` clears it.
    ProjectCommand {
        project: std::path::PathBuf,
        check: bool,
        command: Option<String>,
    },
    /// A project's agents run sandboxed, or not.
    ProjectSandbox {
        project: std::path::PathBuf,
        on: bool,
    },
    /// The variables a project's agents get even though they look like secrets.
    ProjectPassEnv {
        project: std::path::PathBuf,
        names: Vec<String>,
    },
    /// A project's saved secret `id` has a new name.
    ProjectSecretRename {
        project: std::path::PathBuf,
        id: String,
        name: String,
    },
    /// A project's agents start with saved secret `id` in variable `env`, or
    /// without it for `None`.
    ProjectSecretEnv {
        project: std::path::PathBuf,
        id: String,
        env: Option<String>,
    },
    /// A project's saved secret `id` is removed; its value is gone already.
    ProjectSecretRemove {
        project: std::path::PathBuf,
        id: String,
    },
    /// The branch a project's new environments start from; `None` for origin's default.
    ProjectBase {
        project: std::path::PathBuf,
        base: Option<String>,
    },
    /// Only Linux has the switch.
    #[cfg(target_os = "linux")]
    Background(bool),
    /// Quit and start again, which installs the downloaded update.
    RestartToUpdate,
    OpenAccount,
    OpenExtensions,
    /// Close Settings and open the feedback form.
    SendFeedback,
    /// Close Settings and open the Profile dialog, which has the GitHub sign-in.
    OpenProfile,
    #[cfg(feature = "coderlabs")]
    OpenTasks,
    #[cfg(feature = "coderlabs")]
    DisconnectCoderSpace,
}

/// The project open when the dialog opened, for its Project section.
pub struct ProjectSettings {
    pub path: std::path::PathBuf,
    pub name: SharedString,
    pub setup: Option<String>,
    pub check: Option<String>,
    pub base: Option<String>,
    pub sandbox: bool,
    /// The names in Pass to agents, separated by spaces.
    pub pass_env: String,
    /// The secrets saved for its agents: names only, never a value.
    pub secrets: Vec<crate::agent::saved_secret::Saved>,
    /// The project lives on a server: its commands are not saved here.
    pub remote: bool,
}

/// What a row of Saved secrets is being changed to, by the saved secret's id.
#[derive(Clone, PartialEq, Eq)]
enum SecretEdit {
    Rename(String),
    Replace(String),
    Env(String),
}

impl SecretEdit {
    fn id(&self) -> &str {
        match self {
            SecretEdit::Rename(id) | SecretEdit::Replace(id) | SecretEdit::Env(id) => id,
        }
    }
}

/// A text field of the Project section.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ProjectField {
    Setup,
    Check,
    Base,
}

/// How a saved secret's value shows: never at all.
const MASKED: &str = "••••••••";

/// The variable names typed in Pass to agents: separated by spaces or commas,
/// each once.
fn pass_env_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in text.split(|c: char| c.is_whitespace() || c == ',') {
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    }
    names
}

/// Where a section sits in the list: Arakun's own settings, then the agents'.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Group {
    Arakun,
    Agents,
}

impl Group {
    const ALL: [Group; 2] = [Group::Arakun, Group::Agents];

    fn title(self) -> &'static str {
        match self {
            Group::Arakun => "Arakun",
            Group::Agents => "Agents",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    General,
    Appearance,
    Project,
    Branches,
    Voice,
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
    const ALL: &[Section] = &[
        Section::General,
        Section::Appearance,
        Section::Project,
        Section::Branches,
        Section::Voice,
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

    fn group(self) -> Group {
        if self.agent().is_some() {
            Group::Agents
        } else {
            Group::Arakun
        }
    }

    /// The agent a section is about; the list shows its logo.
    fn agent(self) -> Option<AgentKind> {
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

    fn title(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Appearance => "Appearance",
            Section::Project => "Project",
            Section::Branches => "Branches",
            Section::Voice => "Voice",
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
    fn icon(self) -> &'static str {
        match self {
            Section::General => "settings01",
            Section::Appearance => "paint-board",
            Section::Project => "folder01",
            Section::Branches => "git-branch",
            Section::Voice => "volume-high",
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

    fn description(self) -> &'static str {
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
            Section::Voice => "Droid's questions, read aloud with an ElevenLabs voice.",
            Section::Privacy => {
                "What shows on screen, and what Arakun sends to its PostHog under a random id."
            }
            Section::Account => {
                "An optional sign-in with GitHub, for what Arakun is still trying out with some people, such as Teams. Agents never need it."
            }
            #[cfg(feature = "coderlabs")]
            Section::CoderSpace => "Your pending tasks, inside Arakun.",
            Section::Shortcuts => "Everything you can do without the mouse.",
            Section::Claude => "Claude Code's sign-in and what your plan has left.",
            Section::Codex => "Codex's sign-in: your ChatGPT plan or an OpenAI API key.",
            Section::Droid => "Your Factory plan, and what Droid loads in the visible environment.",
            Section::OpenCode => "The providers OpenCode signs in to, OpenCode Go among them.",
            Section::Devin => "Devin's sign-in: your Devin account or a Windsurf API key.",
            Section::Cursor => "Cursor's sign-in: your Cursor account or a Cursor API key.",
            Section::Antigravity => {
                "Antigravity's sign-in: your Google account or a Gemini API key."
            }
        }
    }

    /// Words the search matches besides the title.
    fn keywords(self) -> &'static str {
        match self {
            #[cfg(feature = "coderlabs")]
            Section::General => {
                "raccoon mascot animation motion new session dynamic island notch top bar agents alerts background close hide tray oye arakun wake word voice microphone"
            }
            #[cfg(not(feature = "coderlabs"))]
            Section::General => {
                "raccoon mascot animation motion new session dynamic island notch top bar agents alerts background close hide tray sidebar usage card plans browser web page drive click type scripts"
            }
            Section::Appearance => {
                "tabs sessions pinned pin title bar tree sidebar width group project classic focused everything fold folded hidden long titles two lines sort sorting order latest newest recent alphabetical a-z state icon dot word theme palette color scheme colors accent tint corners radius rounded round sharp spacing gap padding compact roomy density window custom share night graphite midnight forest rose diff colors colorblind high contrast glass blur transparent translucent grays gray text size font scale design system mode dark light zoom bigger smaller reduce motion animation accessibility still"
            }
            Section::Project => {
                "setup script install new environment worktree check command test lint build verify \
                 base branch start from origin main default sandbox seatbelt bubblewrap isolate \
                 security secrets pass to agents variables env token gh_token keys \
                 saved secrets api key password keyring rename replace remove"
            }
            Section::Branches => {
                "branch name naming environment worktree rename model haiku agent merge merged \
                 archive settle resolve threads review notifications pull request cleanup clean up \
                 remove delete old stale environments"
            }
            Section::Voice => {
                "elevenlabs speech speak tts read aloud audio sound api key questions tone"
            }
            Section::Privacy => {
                "telemetry analytics usage counts crash reports privacy posthog data redacted redact hide secrets secret keys tokens mask reveal streamer stream demo screen share sharing incognito feedback bug report idea"
            }
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
        }
    }

    /// Whether the search `query` (lower case, trimmed) leaves this section.
    fn matches(self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        // The Updates card and the link setting are in General in every build, so
        // their words are added here rather than to each build's keyword list.
        let general = if self == Section::General {
            "update updates automatic automatically auto version release upgrade nightly channel beta testers \
             links open system default external \
             sidebar views extensions rail github notes memory usage cloudflare posthog linear doop isometric phone servers automations"
        } else {
            ""
        };
        let text = format!(
            "{} {} {} {general}",
            self.title(),
            self.description(),
            self.keywords()
        )
        .to_lowercase();
        query.split_whitespace().all(|w| text.contains(w))
            || (self == Section::Shortcuts
                && crate::keymap::sheet()
                    .iter()
                    .any(|line| SettingsDialog::shortcut_matches(query, line)))
    }
}

/// The section the last dialog was left on.
static LAST_SECTION: AtomicU8 = AtomicU8::new(0);

/// Makes the next dialog open on General, where the link setting is.
pub fn start_on_general() {
    start_on(Section::General);
}

/// Makes the next dialog open on Branches, where the environment cleanup is.
pub fn start_on_branches() {
    start_on(Section::Branches);
}

fn start_on(section: Section) {
    let ix = Section::ALL.iter().position(|s| *s == section).unwrap_or(0);
    LAST_SECTION.store(ix as u8, Ordering::Relaxed);
}

/// The queue key of the "Test voice" sample.
const TEST_KEY: &str = "settings-test";
const DIALOG_W: f32 = 880.;
/// The section list's column, search included.
const LIST_W: f32 = 224.;
/// Under Claude Code's API key or endpoint, which needs no claude.ai sign-in.
const ACCESS_HINT: &str = "Claude Code runs on this, from ~/.claude/settings.json or the environment. Sign in only to see claude.ai plan limits.";
/// The list and the pane under the header. A short window shrinks it.
const BODY_H: f32 = 560.;
/// A row of the section list, as tall as one of the rail's.
const NAV_ROW_H: f32 = 32.;

pub struct SettingsDialog {
    arcade: bool,
    /// Whether a Factory API key is connected, for Droid's dot in the list.
    droid: bool,
    /// The Teams sign-in, for the Account section.
    teams: Entity<crate::arakun_teams::Teams>,
    /// Who is signed in to CoderSpace, if anyone.
    #[cfg(feature = "coderlabs")]
    coderspace: Option<String>,
    /// The claude.ai plan usage, when Claude Code is signed in.
    claude: Option<PlanUsage>,
    /// The API key or endpoint Claude Code runs on instead of a plan, if any.
    claude_access: Option<claude_auth::Access>,
    /// The providers OpenCode has credentials for.
    opencode: Vec<String>,
    /// How Codex is signed in, if it is.
    codex: Option<&'static str>,
    /// The variable with Codex's API key, when the environment has one.
    codex_env: Option<&'static str>,
    /// Whether `devin auth login` has signed in.
    devin: bool,
    /// The variable with Devin's API key, when the environment has one.
    devin_env: Option<&'static str>,
    /// Whether `cursor-agent login` has signed in.
    cursor: bool,
    /// The variable with Cursor's API key, when the environment has one.
    cursor_env: Option<&'static str>,
    /// Whether agy keeps its sign-in in a file; a keyring sign-in reads as `false`.
    antigravity: bool,
    /// `GEMINI_API_KEY`, when the environment has it.
    antigravity_env: Option<&'static str>,
    input: Entity<TextInput>,
    section: Section,
    voice: voice::Settings,
    /// Whether the keyring holds an ElevenLabs key; `None` while it is being read.
    voice_key: Option<bool>,
    key_input: Entity<TextInput>,
    voice_input: Entity<TextInput>,
    /// What the last save or test did, and whether it failed.
    voice_note: Option<(SharedString, bool)>,
    voice_busy: bool,
    /// What Colors and Shape are set to, for the Appearance summary.
    look: crate::customize_view::Summary,
    /// The nightly switch was clicked and its warning is showing.
    confirm_nightly: bool,
    /// The settings pane's scroll, so a slider that moves the layout can stay
    /// under the pointer.
    pane_scroll: gpui::ScrollHandle,
    /// Who names the branch of a new environment after its first prompt.
    namer: env_name::Namer,
    namer_input: Entity<TextInput>,
    /// Why the last crash report or usage count choice could not be saved.
    telemetry_error: Option<SharedString>,
    /// The extension views the sidebar shows.
    views_on: Vec<crate::sidebar::View>,
    /// The open project, if there is one.
    project: Option<ProjectSettings>,
    setup_input: Entity<TextInput>,
    check_input: Entity<TextInput>,
    base_input: Entity<TextInput>,
    pass_input: Entity<TextInput>,
    /// The saved secret being renamed or replaced, and the field for it.
    secret_edit: Option<SecretEdit>,
    secret_name_input: Entity<TextInput>,
    secret_value_input: Entity<TextInput>,
    secret_env_input: Entity<TextInput>,
    /// What the last change to a saved secret did, and whether it failed.
    secret_note: Option<(SharedString, bool)>,
    /// The sidebar's width, which the Tabs and sessions part sets too.
    pub rail_w: f32,
    _subs: [Subscription; 12],
}

impl EventEmitter<SettingsEvent> for SettingsDialog {}

impl SettingsDialog {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        arcade: bool,
        droid: bool,
        teams: Entity<crate::arakun_teams::Teams>,
        look: crate::customize_view::Summary,
        #[cfg(feature = "coderlabs")] coderspace: Option<String>,
        views_on: Vec<crate::sidebar::View>,
        project: Option<ProjectSettings>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Shortcuts match too; the list's column is too narrow to say so.
        let input = cx.new(|cx| TextInput::new("Search settings", cx));
        let sub = cx.subscribe_in(&input, window, Self::on_input);
        window.focus(&input.focus_handle(cx), cx);
        let section =
            Section::ALL[LAST_SECTION.load(Ordering::Relaxed) as usize % Section::ALL.len()];

        let voice = voice::settings();
        let key_input = cx.new(|cx| TextInput::new("sk_…", cx).mono().secret());
        let voice_input = cx.new(|cx| {
            let mut i = TextInput::new("Paste the voice ID", cx).mono();
            i.set_text(voice.voice_id.clone(), cx);
            i
        });
        let key_sub = cx.subscribe_in(
            &key_input,
            window,
            |this: &mut Self, _, ev, _, cx| match ev {
                InputEvent::Submit => this.save_key(cx),
                InputEvent::Cancel => cx.emit(SettingsEvent::Dismissed),
                _ => {}
            },
        );
        let voice_sub =
            cx.subscribe_in(
                &voice_input,
                window,
                |this: &mut Self, _, ev, _, cx| match ev {
                    InputEvent::Changed => this.voice_id_changed(cx),
                    InputEvent::Cancel => cx.emit(SettingsEvent::Dismissed),
                    _ => {}
                },
            );
        let namer = env_name::namer();
        let namer_input = cx.new(|cx| {
            let mut i = TextInput::new("CLI default", cx).mono();
            i.set_text(namer.model.clone(), cx);
            i
        });
        let namer_sub =
            cx.subscribe_in(
                &namer_input,
                window,
                |this: &mut Self, _, ev, _, cx| match ev {
                    InputEvent::Changed => {
                        let model = this.namer_input.read(cx).text().trim().to_string();
                        if model != this.namer.model {
                            this.namer.model = model;
                            env_name::set_namer(&this.namer);
                        }
                    }
                    InputEvent::Cancel => cx.emit(SettingsEvent::Dismissed),
                    _ => {}
                },
            );
        let setup_input = cx.new(|cx| {
            let mut i = TextInput::new("npm install && cp .env.example .env", cx)
                .mono()
                .redacted();
            if let Some(c) = project.as_ref().and_then(|p| p.setup.as_ref()) {
                i.set_text(c.clone(), cx);
            }
            i
        });
        let check_input = cx.new(|cx| {
            let mut i = TextInput::new("cargo test", cx).mono().redacted();
            if let Some(c) = project.as_ref().and_then(|p| p.check.as_ref()) {
                i.set_text(c.clone(), cx);
            }
            i
        });
        let base_input = cx.new(|cx| {
            let mut i = TextInput::new("origin's default branch", cx).mono();
            if let Some(b) = project.as_ref().and_then(|p| p.base.as_ref()) {
                i.set_text(b.clone(), cx);
            }
            i
        });
        let pass_input = cx.new(|cx| {
            let mut i = TextInput::new("GH_TOKEN NPM_TOKEN", cx).mono();
            if let Some(p) = project.as_ref().filter(|p| !p.pass_env.is_empty()) {
                i.set_text(p.pass_env.clone(), cx);
            }
            i
        });
        let pass_sub = cx.subscribe_in(&pass_input, window, |this: &mut Self, _, ev, _, cx| {
            this.on_pass_input(ev, cx)
        });
        let secret_name_input = cx.new(|cx| TextInput::new("Name", cx));
        let secret_value_input = cx.new(|cx| TextInput::new("Paste the new value", cx).secret());
        let secret_env_input = cx.new(|cx| TextInput::new("STRIPE_KEY", cx).mono());
        let secret_subs =
            [&secret_name_input, &secret_value_input, &secret_env_input].map(|input| {
                cx.subscribe_in(input, window, |this: &mut Self, _, ev, _, cx| match ev {
                    InputEvent::Submit => this.finish_secret_edit(cx),
                    InputEvent::Cancel => this.cancel_secret_edit(cx),
                    _ => {}
                })
            });
        let [secret_name_sub, secret_value_sub, secret_env_sub] = secret_subs;
        let setup_sub = cx.subscribe_in(&setup_input, window, |this: &mut Self, _, ev, _, cx| {
            this.on_project_input(ProjectField::Setup, ev, cx)
        });
        let check_sub = cx.subscribe_in(&check_input, window, |this: &mut Self, _, ev, _, cx| {
            this.on_project_input(ProjectField::Check, ev, cx)
        });
        let base_sub = cx.subscribe_in(&base_input, window, |this: &mut Self, _, ev, _, cx| {
            this.on_project_input(ProjectField::Base, ev, cx)
        });
        cx.spawn(async move |this, cx| {
            let (plan, access) = cx
                .background_spawn(crate::guard::task(async move {
                    (plan::refresh(), claude_auth::access())
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.claude = plan;
                this.claude_access = access;
                cx.notify();
            });
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let set = cx
                .background_spawn(crate::guard::task(
                    async move { voice::api_key().is_some() },
                ))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.voice_key = Some(set);
                cx.notify();
            });
        })
        .detach();

        teams.update(cx, |t, cx| {
            t.refresh_if_unknown(cx);
            t.refresh_if_stale(cx);
        });
        let teams_sub = cx.observe(&teams, |_, _, cx| cx.notify());

        Self {
            arcade,
            droid,
            teams,
            #[cfg(feature = "coderlabs")]
            coderspace,
            claude: None,
            claude_access: None,
            opencode: crate::agent::opencode_providers(),
            codex: crate::agent::codex_sign_in(),
            codex_env: crate::agent::codex_env_key(),
            devin: crate::agent::devin_signed_in(),
            devin_env: crate::agent::devin_env_key(),
            cursor: crate::agent::cursor_signed_in(),
            cursor_env: crate::agent::cursor_env_key(),
            antigravity: crate::agent::antigravity_signed_in(),
            antigravity_env: crate::agent::antigravity_env_key(),
            input,
            section,
            voice,
            voice_key: None,
            key_input,
            voice_input,
            voice_note: None,
            voice_busy: false,
            namer,
            namer_input,
            telemetry_error: None,
            views_on,
            project,
            setup_input,
            check_input,
            base_input,
            pass_input,
            secret_edit: None,
            secret_name_input,
            secret_value_input,
            secret_env_input,
            secret_note: None,
            look,
            rail_w: crate::state::RAIL_W,
            confirm_nightly: false,
            pane_scroll: gpui::ScrollHandle::new(),
            _subs: [
                sub,
                key_sub,
                voice_sub,
                namer_sub,
                teams_sub,
                setup_sub,
                check_sub,
                base_sub,
                pass_sub,
                secret_name_sub,
                secret_value_sub,
                secret_env_sub,
            ],
        }
    }

    /// Saves the project's setup script, check or base branch as it is typed, like
    /// the naming model, so closing the dialog keeps what the field shows.
    fn on_project_input(&mut self, field: ProjectField, ev: &InputEvent, cx: &mut Context<Self>) {
        match ev {
            InputEvent::Changed => {}
            InputEvent::Cancel => return cx.emit(SettingsEvent::Dismissed),
            _ => return,
        }
        let Some(project) = self.project.as_mut().filter(|p| !p.remote) else {
            return;
        };
        let (input, saved) = match field {
            ProjectField::Setup => (&self.setup_input, &mut project.setup),
            ProjectField::Check => (&self.check_input, &mut project.check),
            ProjectField::Base => (&self.base_input, &mut project.base),
        };
        let text = input.read(cx).text().trim().to_string();
        let value = (!text.is_empty()).then_some(text);
        if *saved == value {
            return;
        }
        *saved = value.clone();
        let project = project.path.clone();
        cx.emit(match field {
            ProjectField::Base => SettingsEvent::ProjectBase {
                project,
                base: value,
            },
            _ => SettingsEvent::ProjectCommand {
                project,
                check: field == ProjectField::Check,
                command: value,
            },
        });
    }

    /// Saves the names in Pass to agents as they are typed.
    fn on_pass_input(&mut self, ev: &InputEvent, cx: &mut Context<Self>) {
        match ev {
            InputEvent::Changed => {}
            InputEvent::Cancel => return cx.emit(SettingsEvent::Dismissed),
            _ => return,
        }
        let Some(project) = self.project.as_mut().filter(|p| !p.remote) else {
            return;
        };
        let names = pass_env_names(self.pass_input.read(cx).text());
        let joined = names.join(" ");
        if project.pass_env == joined {
            return;
        }
        project.pass_env = joined;
        cx.emit(SettingsEvent::ProjectPassEnv {
            project: project.path.clone(),
            names,
        });
    }

    fn set_voice_enabled(&mut self, on: bool, cx: &mut Context<Self>) {
        self.voice.enabled = on;
        voice::store(&self.voice);
        if !on {
            voice::stop();
        }
        cx.notify();
    }

    /// Opens the field to rename or replace a saved secret. The value field
    /// starts empty: no screen shows a stored value.
    fn edit_secret(&mut self, edit: SecretEdit, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = &self.project else {
            return;
        };
        let input = match &edit {
            SecretEdit::Rename(id) => {
                let Some(s) = project.secrets.iter().find(|s| &s.id == id) else {
                    return;
                };
                let name = s.name.clone();
                self.secret_name_input
                    .update(cx, |i, cx| i.set_text(name, cx));
                self.secret_name_input.clone()
            }
            SecretEdit::Replace(_) => {
                self.secret_value_input.update(cx, |i, cx| i.clear(cx));
                self.secret_value_input.clone()
            }
            SecretEdit::Env(id) => {
                let var = project
                    .secrets
                    .iter()
                    .find(|s| &s.id == id)
                    .and_then(|s| s.env.clone())
                    .unwrap_or_default();
                self.secret_env_input
                    .update(cx, |i, cx| i.set_text(var, cx));
                self.secret_env_input.clone()
            }
        };
        window.focus(&input.focus_handle(cx), cx);
        self.secret_edit = Some(edit);
        self.secret_note = None;
        cx.notify();
    }

    fn cancel_secret_edit(&mut self, cx: &mut Context<Self>) {
        self.secret_value_input.update(cx, |i, cx| i.clear(cx));
        self.secret_edit = None;
        cx.notify();
    }

    /// Saves the new name, or the new value to the keyring.
    fn finish_secret_edit(&mut self, cx: &mut Context<Self>) {
        let (Some(edit), Some(project)) = (self.secret_edit.clone(), self.project.as_mut()) else {
            return;
        };
        match edit {
            SecretEdit::Rename(id) => {
                let typed = self.secret_name_input.read(cx).text().to_string();
                match crate::agent::saved_secret::check_name(&typed, &project.secrets, Some(&id)) {
                    Ok(name) => {
                        if let Some(s) = project.secrets.iter_mut().find(|s| s.id == id) {
                            s.name = name.clone();
                        }
                        cx.emit(SettingsEvent::ProjectSecretRename {
                            project: project.path.clone(),
                            id,
                            name,
                        });
                        self.secret_edit = None;
                        self.secret_note = None;
                    }
                    Err(e) => self.secret_note = Some((e.into(), true)),
                }
            }
            SecretEdit::Env(id) => {
                let typed = self.secret_env_input.read(cx).text().to_string();
                let env = crate::agent::saved_secret::check_env(&typed).and_then(|env| {
                    let taken = project
                        .secrets
                        .iter()
                        .any(|s| s.id != id && env.is_some() && s.env == env);
                    if taken {
                        return Err("Another saved secret is set as this variable.".into());
                    }
                    Ok(env)
                });
                match env {
                    Ok(env) => {
                        if let Some(s) = project.secrets.iter_mut().find(|s| s.id == id) {
                            s.env = env.clone();
                        }
                        cx.emit(SettingsEvent::ProjectSecretEnv {
                            project: project.path.clone(),
                            id,
                            env,
                        });
                        self.secret_edit = None;
                        self.secret_note = None;
                    }
                    Err(e) => self.secret_note = Some((e.into(), true)),
                }
            }
            SecretEdit::Replace(id) => {
                let Some(saved) = project.secrets.iter().find(|s| s.id == id).cloned() else {
                    return;
                };
                let value = self.secret_value_input.read(cx).text().to_string();
                if value.is_empty() {
                    return;
                }
                self.secret_value_input.update(cx, |i, cx| i.clear(cx));
                self.secret_edit = None;
                cx.spawn(async move |this, cx| {
                    let name = saved.name.clone();
                    let done = cx
                        .background_spawn(crate::guard::task(async move {
                            crate::agent::saved_secret::replace(&saved, &value)
                        }))
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.secret_note = Some(match done {
                            Ok(()) => (format!("Replaced the value of {name}.").into(), false),
                            Err(e) => (format!("Could not replace it: {e}").into(), true),
                        });
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// Removes a saved secret: its value from the keyring, its name from the
    /// project.
    fn remove_secret(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(project) = self.project.as_mut() else {
            return;
        };
        let Some(at) = project.secrets.iter().position(|s| s.id == id) else {
            return;
        };
        let saved = project.secrets.remove(at);
        cx.emit(SettingsEvent::ProjectSecretRemove {
            project: project.path.clone(),
            id: saved.id.clone(),
        });
        if self
            .secret_edit
            .as_ref()
            .is_some_and(|e| e.id() == saved.id)
        {
            self.cancel_secret_edit(cx);
        }
        self.secret_note = Some((format!("Removed {}.", saved.name).into(), false));
        cx.background_spawn(crate::guard::task(async move {
            crate::agent::saved_secret::remove(&saved);
        }))
        .detach();
        cx.notify();
    }

    /// Settings › Project › Saved secrets: each one by name, its value
    /// masked, with Rename, Replace and Remove.
    fn render_saved_secrets(
        &self,
        project: &ProjectSettings,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        if project.secrets.is_empty() {
            rows.push(
                row(
                    "None yet",
                    "When an agent asks for a secret, turn on Save for this project on its card. Agents of this project can then ask for it again by name, and get a one-use handle, never the value. Variable sets one as an environment variable for them.",
                    div().into_any_element(),
                )
                .into_any_element(),
            );
        }
        for (i, s) in project.secrets.iter().enumerate() {
            let editing = self.secret_edit.as_ref().filter(|e| e.id() == s.id);
            let control = match editing {
                Some(edit) => {
                    let input = match edit {
                        SecretEdit::Rename(_) => self.secret_name_input.clone(),
                        SecretEdit::Replace(_) => self.secret_value_input.clone(),
                        SecretEdit::Env(_) => self.secret_env_input.clone(),
                    };
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .child(field(input, 220.))
                        .child(
                            ui::button(("set-secret-done", i), "Save", ButtonKind::Outline, true)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.finish_secret_edit(cx)),
                                ),
                        )
                        .child(
                            ui::button(("set-secret-cancel", i), "Cancel", ButtonKind::Ghost, true)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.cancel_secret_edit(cx)),
                                ),
                        )
                }
                None => {
                    let (rename, replace, remove, env) =
                        (s.id.clone(), s.id.clone(), s.id.clone(), s.id.clone());
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .child(
                            ui::button(("set-secret-rename", i), "Rename", ButtonKind::Ghost, true)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.edit_secret(SecretEdit::Rename(rename.clone()), window, cx)
                                })),
                        )
                        .child(
                            ui::button(("set-secret-env", i), "Variable", ButtonKind::Ghost, true)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.edit_secret(SecretEdit::Env(env.clone()), window, cx)
                                })),
                        )
                        .child(
                            ui::button(
                                ("set-secret-replace", i),
                                "Replace",
                                ButtonKind::Outline,
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.edit_secret(
                                        SecretEdit::Replace(replace.clone()),
                                        window,
                                        cx,
                                    )
                                },
                            )),
                        )
                        .child(
                            ui::button(
                                ("set-secret-remove", i),
                                "Remove",
                                ButtonKind::Danger,
                                true,
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.remove_secret(&remove, cx)),
                            ),
                        )
                }
            };
            let hint = match (&s.env, editing) {
                (_, Some(SecretEdit::Env(_))) => "The variable this project's agents start with it in, such as STRIPE_KEY. Empty sets none. Applies to sessions started afterwards.".to_string(),
                (Some(var), _) => format!("{MASKED}  ·  ${var} for this project's agents"),
                (None, _) => MASKED.to_string(),
            };
            rows.push(row(s.name.clone(), hint, control.into_any_element()).into_any_element());
        }
        let mut parts = vec![titled("Saved secrets", rows)];
        if let Some((text, failed)) = &self.secret_note {
            parts.push(
                div()
                    .px(crate::ui::sp(4.))
                    .text_size(px(ui::TEXT_SM))
                    .text_color(if *failed {
                        theme::danger().into()
                    } else {
                        theme::text_subtle()
                    })
                    .child(text.clone())
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .children(parts)
            .into_any_element()
    }

    fn save_key(&mut self, cx: &mut Context<Self>) {
        let key = self.key_input.read(cx).text().trim().to_string();
        if key.is_empty() || self.voice_busy {
            return;
        }
        self.voice_busy = true;
        self.voice_note = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_spawn(crate::guard::task(
                    async move { voice::store_api_key(&key) },
                ))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.voice_busy = false;
                match saved {
                    Ok(()) => {
                        this.voice_key = Some(true);
                        this.key_input.update(cx, |i, cx| i.set_text("", cx));
                        this.voice_note =
                            Some(("Key saved. Try it with Test voice.".into(), false));
                    }
                    Err(e) => {
                        this.voice_note =
                            Some((format!("Could not save the key: {e}").into(), true))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_key(&mut self, cx: &mut Context<Self>) {
        if self.voice_busy {
            return;
        }
        self.voice_busy = true;
        cx.spawn(async move |this, cx| {
            cx.background_spawn(crate::guard::task(async move { voice::clear_api_key() }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.voice_busy = false;
                this.voice_key = Some(false);
                this.voice_note = Some(("Key removed from the system keyring.".into(), false));
                cx.notify();
            });
        })
        .detach();
    }

    /// Picks the agent that names branches; a new agent starts on its small model.
    fn set_namer_choice(&mut self, choice: env_name::Choice, cx: &mut Context<Self>) {
        if choice == self.namer.choice {
            return;
        }
        self.namer.choice = choice;
        if let Some(agent) = choice.agent() {
            self.namer.model = env_name::default_model(agent).to_string();
            let model = self.namer.model.clone();
            self.namer_input.update(cx, |i, cx| i.set_text(model, cx));
        }
        env_name::set_namer(&self.namer);
        cx.notify();
    }

    fn set_tone(&mut self, tone: Tone, cx: &mut Context<Self>) {
        self.voice.tone = tone;
        voice::store(&self.voice);
        cx.notify();
    }

    fn voice_id_changed(&mut self, cx: &mut Context<Self>) {
        let id = self.voice_input.read(cx).text().trim().to_string();
        if (id.is_empty() || voice::valid_voice_id(&id)) && id != self.voice.voice_id {
            self.voice.voice_id = id;
            voice::store(&self.voice);
        }
        cx.notify();
    }

    fn test_voice(&mut self, cx: &mut Context<Self>) {
        if self.voice_busy {
            return;
        }
        let id = self.voice.voice_id.clone();
        let tone = self.voice.tone;
        self.voice_busy = true;
        self.voice_note = None;
        voice::cancel(TEST_KEY);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let wav = cx
                .background_spawn(crate::guard::task(async move {
                    let key = voice::api_key()
                        .ok_or_else(|| "Save your ElevenLabs API key first.".to_string())?;
                    voice::fetch(&key, &id, tone, voice::SAMPLE)
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.voice_busy = false;
                match wav {
                    Ok(path) => voice::play(TEST_KEY, path),
                    Err(e) => this.voice_note = Some((e.into(), true)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn on_input(
        &mut self,
        _: &Entity<TextInput>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Changed => {
                let shown = self.shown(cx);
                if !shown.contains(&self.section)
                    && let Some(&first) = shown.first()
                {
                    self.section = first;
                }
                cx.notify();
            }
            InputEvent::Up => self.move_section(-1, cx),
            InputEvent::Down | InputEvent::Complete => self.move_section(1, cx),
            InputEvent::Cancel => {
                if self.query(cx).is_empty() {
                    cx.emit(SettingsEvent::Dismissed);
                } else {
                    self.input.update(cx, |i, cx| i.set_text("", cx));
                }
            }
            InputEvent::Submit | InputEvent::SubmitAlt => {}
        }
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).text().trim().to_lowercase()
    }

    fn shortcut_matches(query: &str, line: &crate::keymap::Line) -> bool {
        let keys = Self::shortcut_keys(line);
        query.is_empty()
            || line.label.to_lowercase().contains(query)
            || keys.to_lowercase().contains(query)
            || crate::platform::shortcut_label(&keys)
                .to_lowercase()
                .contains(query)
            || line.group.to_lowercase().contains(query)
    }

    /// A line's keys as `keycaps` reads them: "Ctrl+K / Ctrl+P".
    fn shortcut_keys(line: &crate::keymap::Line) -> String {
        crate::keymap::key_labels(&line.keys, true).join(" / ")
    }

    /// The sections the search leaves, in order.
    fn shown(&self, cx: &App) -> Vec<Section> {
        let q = self.query(cx);
        Section::ALL
            .iter()
            .copied()
            .filter(|s| s.matches(&q))
            .collect()
    }

    /// Moves an open dialog to Branches; its pending edits stay.
    pub fn show_branches(&mut self, cx: &mut Context<Self>) {
        // A search that leaves Branches out would show its own empty page.
        if !Section::Branches.matches(&self.query(cx)) {
            self.input.update(cx, |input, cx| input.set_text("", cx));
        }
        self.select(Section::Branches, cx);
    }

    fn select(&mut self, s: Section, cx: &mut Context<Self>) {
        self.section = s;
        let ix = Section::ALL.iter().position(|x| *x == s).unwrap_or(0);
        LAST_SECTION.store(ix as u8, Ordering::Relaxed);
        cx.notify();
    }

    fn move_section(&mut self, delta: isize, cx: &mut Context<Self>) {
        let shown = self.shown(cx);
        if shown.is_empty() {
            return;
        }
        let at = shown.iter().position(|s| *s == self.section).unwrap_or(0) as isize;
        let next = (at + delta).rem_euclid(shown.len() as isize) as usize;
        self.select(shown[next], cx);
    }

    /// A sign-in in `~/.codex` comes first, as it does for the adapter; a key
    /// only in the environment is named by its variable, never shown.
    fn codex_status(&self) -> String {
        match (self.codex, self.codex_env) {
            (Some(how), _) => format!("Signed in · {how}"),
            (None, Some(var)) => format!("API key · {var}"),
            (None, None) => "Not signed in".into(),
        }
    }

    /// Devin's own sign-in comes first; a key only in the environment is named
    /// by its variable, never shown.
    fn devin_status(&self) -> String {
        match (self.devin, self.devin_env) {
            (true, _) => "Signed in".into(),
            (false, Some(var)) => format!("API key · {var}"),
            (false, None) => "Not signed in".into(),
        }
    }

    /// Cursor's own sign-in comes first; a key only in the environment is named
    /// by its variable, never shown.
    fn cursor_status(&self) -> String {
        match (self.cursor, self.cursor_env) {
            (true, _) => "Signed in".into(),
            (false, Some(var)) => format!("API key · {var}"),
            (false, None) => "Not signed in".into(),
        }
    }

    /// agy's own sign-in comes first; the key is named by its variable. With no
    /// file, agy may still be signed in through the system keyring.
    fn antigravity_status(&self) -> String {
        match (self.antigravity, self.antigravity_env) {
            (true, _) => "Signed in".into(),
            (false, Some(var)) => format!("API key · {var}"),
            (false, None) => "No sign-in found".into(),
        }
    }

    /// Whether an agent's section gets the green dot: it can run, through a
    /// sign-in or a key. `false` for the other sections.
    fn signed_in(&self, s: Section) -> bool {
        match s {
            Section::Claude => self.claude.is_some() || self.claude_access.is_some(),
            Section::Codex => self.codex.is_some() || self.codex_env.is_some(),
            Section::Droid => self.droid,
            Section::OpenCode => !self.opencode.is_empty(),
            Section::Devin => self.devin || self.devin_env.is_some(),
            Section::Cursor => self.cursor || self.cursor_env.is_some(),
            Section::Antigravity => self.antigravity || self.antigravity_env.is_some(),
            _ => false,
        }
    }

    /// One line of the section list: its icon (an agent's logo), its name,
    /// and a dot when the agent is signed in.
    fn render_row(&self, s: Section, cx: &mut Context<Self>) -> AnyElement {
        let selected = s == self.section;
        let lead = match s.agent() {
            Some(agent) => agent.icon(14.),
            None => icon(s.icon(), 14.)
                .text_color(if selected {
                    theme::text()
                } else {
                    theme::text_subtle()
                })
                .into_any_element(),
        };
        div()
            .id(s.title())
            .role(gpui::Role::Tab)
            .aria_label(s.title())
            .aria_selected(selected)
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(8.))
            .h(px(NAV_ROW_H))
            .px(crate::ui::sp(8.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .text_size(px(ui::TEXT_MD))
            .cursor_pointer()
            .when(selected, |d| {
                d.bg(theme::selection()).text_color(theme::text())
            })
            .when(!selected, |d| {
                d.text_color(theme::text_muted())
                    .hover(|st| st.bg(theme::fill_hover()))
            })
            .child(lead)
            .child(div().flex_1().min_w_0().truncate().child(s.title()))
            .when(self.signed_in(s), |d| {
                d.child(
                    div()
                        .flex_none()
                        .size(px(6.))
                        .rounded_full()
                        .bg(theme::success()),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| this.select(s, cx)))
            .into_any_element()
    }

    fn render_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let s = self.section;
        let head = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(crate::ui::sp(2.))
            .child(div().ty(ui::Type::Heading).child(s.title()))
            .child(
                div()
                    .text_size(px(ui::TEXT_SM))
                    .line_height(px(16.))
                    .text_color(theme::text_subtle())
                    .child(s.description()),
            );
        let body: AnyElement = match s {
            Section::General => {
                let raccoon = vec![
                    toggle_row("set-arcade", "Raccoon animation", "Follows the cursor and acts out what agents do. Off, it holds still.", self.arcade)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.arcade = !this.arcade;
                        cx.emit(SettingsEvent::Arcade(this.arcade));
                        cx.notify();
                    }))
                    .into_any_element(),
                    toggle_row("set-island", "Dynamic Island", "What every agent is doing, at the top of the screen, and a card when one finishes, needs you or fails. On Linux it needs Wayland.", crate::island::enabled())
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(SettingsEvent::Island(!crate::island::enabled()));
                        cx.notify();
                    }))
                    .into_any_element(),
                    #[cfg(target_os = "linux")]
                    toggle_row("set-background", "Keep running in the background", "Closing the window hides it; agents, terminals and the island go on. Run arakun to bring it back, arakun quit to quit.", crate::background::enabled())
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(SettingsEvent::Background(!crate::background::enabled()));
                        cx.notify();
                    }))
                    .into_any_element(),
                    #[cfg(feature = "coderlabs")]
                    toggle_row("set-wake", "Oye Arakun", "The island listens for \"Oye Arakun\" to start a call, on this computer. Off, it doesn't use the microphone until you call it yourself.", crate::island_host::wake::enabled())
                    .on_click(cx.listener(|_, _, _, cx| {
                        crate::island_host::wake::set_enabled(!crate::island_host::wake::enabled());
                        cx.notify();
                    }))
                    .into_any_element(),
                ];
                let mut chips = div().flex().flex_wrap().items_center().gap(crate::ui::sp(6.));
                for (i, agent) in AgentKind::WITH_PLAN.into_iter().enumerate() {
                    let on = crate::account_view::rail_shows(agent);
                    chips = chips.child(
                        ui::chip(("set-rail-usage", i), agent.label(), on).on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(SettingsEvent::RailUsage(agent, !on));
                            cx.notify();
                        })),
                    );
                }
                let mut views = div().flex().flex_wrap().items_center().gap(crate::ui::sp(6.));
                for (i, view) in crate::sidebar::View::EXTENSIONS.iter().copied().enumerate() {
                    let on = self.views_on.contains(&view);
                    views = views.child(
                        ui::chip(("set-sidebar-view", i), view.label(), on).on_click(cx.listener(move |this, _, _, cx| {
                            if on {
                                this.views_on.retain(|v| *v != view);
                            } else {
                                this.views_on.push(view);
                            }
                            cx.emit(SettingsEvent::SidebarView(view, !on));
                            cx.notify();
                        })),
                    );
                }
                stack(vec![
                    titled("Raccoon", raccoon),
                    titled(
                        "Sidebar",
                        vec![
                            stacked_row(
                                "Views",
                                "The extra views the sidebar shows. The Marketplace has the same switches, with what each view does.",
                                views.into_any_element(),
                            )
                            .into_any_element(),
                            stacked_row(
                                "Usage card",
                                "The plans the card at the bottom of the sidebar shows; OpenCode's once a Go key is connected. The Usage tab always shows them all.",
                                chips.into_any_element(),
                            )
                            .into_any_element(),
                        ],
                    ),
                    titled("Updates", self.update_rows(cx)),
                    titled(
                        "Browser",
                        vec![
                            toggle_row("set-links-in-arakun", "Open links in Arakun", if crate::browser::ENABLED {
                                    "Links from sessions, the GitHub view and pins open in a browser tab. Off, they open in your own browser."
                                } else {
                                    "This build has no browser tabs, so leave it off: links then open in your own browser."
                                }, crate::browser::links_in_arakun())
                            .on_click(cx.listener(|_, _, _, cx| {
                                crate::browser::set_links_in_arakun(!crate::browser::links_in_arakun());
                                cx.notify();
                            }))
                            .into_any_element(),
                            toggle_row("set-drive-pages", "Agents drive any web page", "Click, type and run scripts in any tab, with your signed-in accounts. Off, they drive local pages and only read the rest.", crate::page_drive::any_page())
                            .on_click(cx.listener(|_, _, _, cx| {
                                crate::page_drive::set_any_page(!crate::page_drive::any_page());
                                cx.notify();
                            }))
                            .into_any_element(),
                        ],
                    ),
                ])
            }
            Section::Appearance => self.render_appearance(cx),
            Section::Project => self.render_project(cx),
            Section::Branches => {
                let current = self.namer.choice;
                let mut choices = ui::segmented();
                for (i, choice) in env_name::Choice::ALL.into_iter().enumerate() {
                    choices = choices.child(
                        ui::segment(("set-namer", i), None, choice.label(), choice == current)
                            .on_click(cx.listener(move |this, _, _, cx| this.set_namer_choice(choice, cx))),
                    );
                }
                let current_days = crate::cleanup::days().unwrap_or(0);
                let mut days = ui::segmented();
                for (i, d) in crate::cleanup::CHOICES.into_iter().enumerate() {
                    days = days.child(
                        ui::segment(
                            ("set-env-cleanup", i),
                            None,
                            crate::cleanup::label(d),
                            d == current_days,
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            crate::cleanup::set_days(d);
                            cx.notify();
                        })),
                    );
                }
                let cleanup = stacked_row(
                    "Remove old environments",
                    "Removes an environment's folder once its pull request is merged or closed and nobody opened it for this long. Environments with uncommitted files, commits that aren't in the pull request or a working agent stay. The branch is kept.",
                    days.into_any_element(),
                )
                .into_any_element();
                let settle = toggle_row("set-settle-after-merge", "Archive after merge", "Once an environment's pull request is merged: its open review threads are resolved on GitHub, its GitHub notifications are marked read, and the environment's sessions stop asking for you and are archived. The GitHub tab marks read the GitHub notifications of every merged pull request.", crate::settle::on())
                .on_click(cx.listener(|_, _, _, cx| {
                    crate::settle::set(!crate::settle::on());
                    cx.notify();
                }))
                .into_any_element();
                stack(vec![titled(
                    "Naming",
                    vec![
                        stacked_row(
                            "Name new branches",
                            "After the first prompt, an agent names the branch after the task and Arakun renames it. The folder keeps its name.",
                            choices.into_any_element(),
                        )
                        .into_any_element(),
                        row(
                            "Naming model",
                            match self.namer.choice {
                                env_name::Choice::Claude => "A small, fast model, as the CLI names it: haiku, or a full ID. Empty uses the CLI's default. Claude Code names with no tools.",
                                env_name::Choice::Droid => "A small, fast model, as the CLI names it. Empty uses the CLI's default. Droid names read-only: it can read files, not change them.",
                                env_name::Choice::Session => "The agent the prompt went to names it, if it is Claude Code or Droid, so the prompt goes nowhere else: haiku for Claude Code, the CLI's default for Droid.",
                                env_name::Choice::Off => "Naming is off: new branches keep their random name.",
                            },
                            // Only an agent picked here takes a model of its own.
                            if self.namer.choice.agent().is_some() {
                                field(self.namer_input.clone(), 180.).into_any_element()
                            } else {
                                div().into_any_element()
                            },
                        )
                        .into_any_element(),
                    ],
                ),
                    titled("After merge", vec![settle, cleanup]),
                ])
            }
            Section::Privacy => {
                let redacted = toggle_row("set-redacted", "Redacted mode", "For streams, demos and screen sharing: hides account emails, keys and tokens, pairing codes, addresses and your user name, in the views, the terminals and the agents' replies. Ctrl+Shift+H turns it on and off.", crate::redact::on())
                .on_click(cx.listener(|_, _, _, cx| {
                    crate::redact::set(!crate::redact::on());
                    cx.refresh_windows();
                }))
                .into_any_element();
                let secrets = toggle_row("set-hide-secrets", "Hide secrets in sessions", "Keys, tokens and passwords an agent prints show as stars until you click them, and copying one asks first. Redacted mode hides them too, and doesn't show them on a click.", crate::redact::hides_secrets())
                .on_click(cx.listener(|_, _, _, cx| {
                    crate::redact::set_hides_secrets(!crate::redact::hides_secrets());
                    cx.refresh_windows();
                }))
                .into_any_element();
                let mut rows = vec![self.telemetry_row(true, cx), self.telemetry_row(false, cx)];
                if let Some(e) = &self.telemetry_error {
                    rows.push(
                        div()
                            .px(crate::ui::sp(14.))
                            .py(crate::ui::sp(10.))
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::error())
                            .child(e.clone())
                            .into_any_element(),
                    );
                }
                rows.push(
                    row(
                        "Feedback",
                        "A bug, an idea or anything else, sent to the Arakun team when you press Send.",
                        ui::button("set-feedback", "Send Feedback…", ButtonKind::Outline, true)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::SendFeedback)))
                            .into_any_element(),
                    )
                    .into_any_element(),
                );
                stack(vec![
                    titled("On screen", vec![secrets, redacted]),
                    titled("Sent to Arakun", rows),
                ])
            }
            Section::Claude => {
                let signed_in = self.claude.is_some();
                let access = self.claude_access.as_ref();
                let mut rows = vec![row(
                    match (&self.claude, access) {
                        (Some(p), _) => {
                            let label = p.plan_label();
                            if label.is_empty() { "Signed in".to_string() } else { format!("Signed in · {label}") }
                        }
                        (None, Some(a)) => a.label(),
                        (None, None) => "Not signed in".to_string(),
                    },
                    match (signed_in, access) {
                        (true, _) => "The same sign-in Claude Code uses; Arakun reads it from ~/.claude.",
                        (false, Some(_)) => ACCESS_HINT,
                        (false, None) => "Runs claude /login in a terminal; approve the sign-in in the browser.",
                    },
                    if signed_in {
                        ui::button("set-claude-out", "Sign out", ButtonKind::Danger, true)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::ClaudeSignOut)))
                            .into_any_element()
                    } else {
                        ui::button("set-claude-in", "Sign in", ButtonKind::Outline, true)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::ClaudeSignIn)))
                            .into_any_element()
                    },
                )
                .into_any_element()];
                if let (true, Some(a)) = (signed_in, access) {
                    rows.push(
                        row(
                            a.label(),
                            "Claude Code runs on this instead of the plan, from ~/.claude/settings.json or the environment.",
                            div().into_any_element(),
                        )
                        .into_any_element(),
                    );
                }
                if let Some(p) = &self.claude {
                    let windows = p.windows_summary();
                    rows.push(
                        row(
                            "Plan limits",
                            if windows.is_empty() {
                                "No usage windows reported yet. The Usage tab keeps them live.".to_string()
                            } else {
                                format!("{windows} used. The Usage tab keeps them live, with reset times.")
                            },
                            ui::button("set-claude-usage", "Open Usage", ButtonKind::Outline, true)
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::OpenUsage)))
                                .into_any_element(),
                        )
                        .into_any_element(),
                    );
                }
                rows.push(no_sign_in_row(AgentKind::Claude));
                stack(vec![titled("Account", rows)])
            }
            Section::Codex => {
                let signed_in = self.codex.is_some();
                stack(vec![titled(
                    "Account",
                    vec![
                        row(
                            self.codex_status(),
                            if signed_in {
                                "The same sign-in the codex CLI uses; Arakun reads it from ~/.codex."
                            } else if self.codex_env.is_some() {
                                "Sessions use the API key in the environment. Sign in to use your ChatGPT plan instead."
                            } else {
                                "Runs codex login in a terminal; approve the sign-in with ChatGPT in the browser. CODEX_API_KEY or OPENAI_API_KEY works too."
                            },
                            if signed_in {
                                ui::button("set-codex-out", "Sign out", ButtonKind::Danger, true)
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::CodexSignOut)))
                                    .into_any_element()
                            } else {
                                ui::button("set-codex-in", "Sign in", ButtonKind::Outline, true)
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::CodexSignIn)))
                                    .into_any_element()
                            },
                        )
                        .into_any_element(),
                        no_sign_in_row(AgentKind::Codex),
                    ],
                )])
            }
            Section::Devin => {
                let signed_in = self.devin;
                stack(vec![titled(
                    "Account",
                    vec![
                        row(
                            self.devin_status(),
                            if signed_in {
                                "The same sign-in the devin CLI uses. Arakun only checks that it is there."
                            } else if self.devin_env.is_some() {
                                "Sessions use the API key in the environment. Sign in to use your Devin account instead."
                            } else {
                                "Runs devin auth login in a terminal; approve the sign-in in the browser. WINDSURF_API_KEY works too."
                            },
                            if signed_in {
                                ui::button("set-devin-out", "Sign out", ButtonKind::Danger, true)
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::DevinSignOut)))
                                    .into_any_element()
                            } else {
                                ui::button("set-devin-in", "Sign in", ButtonKind::Outline, true)
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::DevinSignIn)))
                                    .into_any_element()
                            },
                        )
                        .into_any_element(),
                        no_sign_in_row(AgentKind::Devin),
                    ],
                )])
            }
            Section::Cursor => {
                let signed_in = self.cursor;
                stack(vec![titled(
                    "Account",
                    vec![
                        row(
                            self.cursor_status(),
                            if signed_in {
                                "The same sign-in the cursor-agent CLI uses. Arakun only checks that it is there."
                            } else if self.cursor_env.is_some() {
                                "Sessions use the API key in the environment. Sign in to use your Cursor account instead."
                            } else {
                                "Runs cursor-agent login in a terminal; approve the sign-in in the browser. CURSOR_API_KEY works too."
                            },
                            if signed_in {
                                ui::button("set-cursor-out", "Sign out", ButtonKind::Danger, true)
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::CursorSignOut)))
                                    .into_any_element()
                            } else {
                                ui::button("set-cursor-in", "Sign in", ButtonKind::Outline, true)
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::CursorSignIn)))
                                    .into_any_element()
                            },
                        )
                        .into_any_element(),
                        no_sign_in_row(AgentKind::Cursor),
                    ],
                )])
            }
            // agy signs out from its own session (`/logout`), so there is only
            // a Sign in button.
            Section::Antigravity => stack(vec![titled(
                "Account",
                vec![
                    row(
                        self.antigravity_status(),
                        if self.antigravity {
                            "The same sign-in the agy CLI uses. Arakun only checks that it is there."
                        } else if self.antigravity_env.is_some() {
                            "Sessions can use the API key in the environment. Sign in to use your Google account instead."
                        } else {
                            "Runs agy acp --login in a terminal; paste the code from the browser. agy may already be signed in through the system keyring, which Arakun doesn't read."
                        },
                        ui::button("set-antigravity-in", "Sign in", ButtonKind::Outline, true)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::AntigravitySignIn)))
                            .into_any_element(),
                    )
                    .into_any_element(),
                    no_sign_in_row(AgentKind::Antigravity),
                ],
            )]),
            Section::Droid => stack(vec![
                titled(
                    "Factory",
                    vec![
                        row(
                            "Plan usage",
                            if self.droid {
                                "Credits used this period, and the API key Arakun reads them with."
                            } else {
                                "Connect a Factory API key to see what your plan has left."
                            },
                            ui::button("set-account", "Open", ButtonKind::Outline, true)
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::OpenAccount)))
                                .into_any_element(),
                        )
                        .into_any_element(),
                        no_sign_in_row(AgentKind::Droid),
                    ],
                ),
                titled(
                    "Extensions",
                    vec![
                        row(
                            "Plugins, skills & MCP servers",
                            "Install Claude SEO, and turn skills and servers on or off for the visible environment.",
                            ui::button("set-extensions", "Manage", ButtonKind::Outline, true)
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::OpenExtensions)))
                                .into_any_element(),
                        )
                        .into_any_element(),
                    ],
                ),
            ]),
            Section::OpenCode => {
                let signed_in = !self.opencode.is_empty();
                let go = self.opencode.iter().any(|p| p == "opencode-go");
                stack(vec![titled(
                    "Providers",
                    vec![
                        row(
                            if go { "OpenCode Go connected".to_string() } else { "OpenCode Go".to_string() },
                            "Copy your API key from the OpenCode console, then Connect: a terminal asks only for the key. Go models show as opencode-go/… in the composer.",
                            div()
                                .flex()
                                .gap(crate::ui::sp(6.))
                                .child(
                                    ui::button_with("set-opencode-go-key", Some("link-square02"), "Get API key", ButtonKind::Ghost, true)
                                        .on_click(|_, _, cx| cx.open_url("https://opencode.ai/auth")),
                                )
                                .child(
                                    ui::button("set-opencode-go", if go { "Replace key" } else { "Connect" }, ButtonKind::Outline, true)
                                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::OpenCodeGoSignIn))),
                                )
                                .into_any_element(),
                        )
                        .into_any_element(),
                        row(
                            if signed_in { format!("Signed in to {}", self.opencode.join(", ")) } else { "Not signed in".to_string() },
                            "Runs opencode auth login in a terminal: pick a provider and paste its API key. Its models show in the composer's model menu.",
                            div()
                                .flex()
                                .gap(crate::ui::sp(6.))
                                .when(signed_in, |d| {
                                    d.child(
                                        ui::button("set-opencode-out", "Sign out", ButtonKind::Danger, true)
                                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::OpenCodeSignOut))),
                                    )
                                })
                                .child(
                                    ui::button("set-opencode-in", if signed_in { "Add provider" } else { "Sign in" }, ButtonKind::Outline, true)
                                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::OpenCodeSignIn))),
                                )
                                .into_any_element(),
                        )
                        .into_any_element(),
                        no_sign_in_row(AgentKind::OpenCode),
                    ],
                )])
            }
            Section::Voice => self.render_voice(cx),
            Section::Account => self.render_account(cx),
            #[cfg(feature = "coderlabs")]
            Section::CoderSpace => {
                let signed_in = self.coderspace.is_some();
                let mut rows = vec![
                    row(
                        match &self.coderspace {
                            Some(who) => format!("Signed in as {who}"),
                            None => "Not signed in".to_string(),
                        },
                        if signed_in {
                            "Your pending tasks live in the Tasks tab."
                        } else {
                            "Sign in with your email from the Tasks tab to see your pending tasks."
                        },
                        ui::button("set-cs-tasks", "Open Tasks", ButtonKind::Outline, true)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::OpenTasks)))
                            .into_any_element(),
                    )
                    .into_any_element(),
                ];
                if signed_in {
                    rows.push(
                        row(
                            "Sign out",
                            "Arakun forgets the CoderSpace session on this machine.",
                            ui::button("set-cs-disconnect", "Sign out", ButtonKind::Danger, true)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.coderspace = None;
                                    cx.emit(SettingsEvent::DisconnectCoderSpace);
                                    cx.notify();
                                }))
                                .into_any_element(),
                        )
                        .into_any_element(),
                    );
                }
                group(rows)
            }
            Section::Shortcuts => self.render_shortcuts(cx),
        };
        div()
            .id("settings-pane")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.pane_scroll)
            .px(crate::ui::sp(24.))
            .pt(crate::ui::sp(16.))
            .pb(crate::ui::sp(24.))
            .gap(crate::ui::sp(24.))
            .child(head)
            .child(body)
            .into_any_element()
    }

    /// The Account section: the GitHub sign-in that Teams uses.
    /// The sign-in lives in the Profile dialog; this row is how Settings,
    /// and its search, still get there.
    fn render_account(&self, cx: &mut Context<Self>) -> AnyElement {
        let teams = self.teams.read(cx);
        let hint = match teams.me() {
            Some(m) if teams.signed_in() => {
                format!(
                    "Signed in as {}.",
                    crate::arakun_teams::account_name(&m.login)
                )
            }
            _ if teams.signed_in() => "Signed in.".to_string(),
            _ => "Not signed in. Sign in from your profile.".to_string(),
        };
        stack(vec![titled(
            "GitHub",
            vec![
                row(
                    "Profile",
                    hint,
                    ui::button("set-profile", "Open Profile", ButtonKind::Outline, true)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::OpenProfile)))
                        .into_any_element(),
                )
                .into_any_element(),
            ],
        )])
    }

    /// The Updates card: the automatic switch, and the version with what
    /// the last check found.
    fn update_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        use crate::update::{self, Status};
        let version = format!("Arakun {}", env!("ARAKUN_VERSION"));
        if !update::release_build() {
            return vec![
                row(
                    version,
                    "Built from source, so it does not update itself. Pull and build it again, or install a release.",
                    div().into_any_element(),
                )
                .into_any_element(),
            ];
        }
        let automatic = update::automatic();
        let auto = toggle_row("set-auto-update", "Update automatically", "Looks for a new release on GitHub every 6 hours and downloads it. It is installed when you quit Arakun.", automatic)
        .on_click(cx.listener(move |_, _, _, cx| {
            update::set_automatic(!automatic);
            cx.notify();
        }))
        .into_any_element();
        let nightly = self.nightly_row(cx);
        let status = update::status();
        let busy = matches!(status, Status::Checking | Status::Downloading(_));
        let action = match &status {
            // Only Linux and macOS can start Arakun again by themselves.
            Status::Ready(tag) if cfg!(unix) && update::will_install(tag) => ui::button(
                "set-update-restart",
                "Restart now",
                ButtonKind::Outline,
                true,
            )
            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::RestartToUpdate))),
            _ => ui::button("set-update-check", "Check now", ButtonKind::Outline, !busy).on_click(
                cx.listener(move |_, _, _, cx| {
                    if busy {
                        return;
                    }
                    update::set_status(Status::Checking);
                    cx.spawn(async move |this, cx| {
                        cx.background_spawn(crate::guard::task(async { update::check() }))
                            .await
                            .ok();
                        this.update(cx, |_, cx| cx.notify()).ok();
                    })
                    .detach();
                    cx.notify();
                }),
            ),
        };
        let current = row(version, status.text(), action.into_any_element()).into_any_element();
        vec![auto, nightly, current]
    }

    /// The nightly channel: a switch that shows what nightlies risk, and asks
    /// again, before it takes them.
    fn nightly_row(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::update::{self, Channel};
        let on = update::channel() == Channel::Nightly;
        let hint = if on {
            "Gets a build of main every night. Turn it off to go back to releases: this build stays until a release newer than it comes out."
        } else {
            "Builds of main from every night, before they are released. For maintainers and testers."
        };
        // Off the UI thread: the write waits while another Arakun installs
        // an update (update::channel_lock).
        let set = |to: Channel, cx: &mut Context<Self>| {
            cx.spawn(async move |this, cx| {
                let saved = cx
                    .background_spawn(crate::guard::task(async move { update::set_channel(to) }))
                    .await;
                if let Err(e) = saved {
                    eprintln!("arakun: the update channel will not last past a restart: {e}");
                }
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        };
        let switch_row = toggle_row("set-nightly", "Nightly builds", hint, on).on_click(
            cx.listener(move |this, _, _, cx| {
                if on {
                    set(Channel::Stable, cx);
                } else {
                    this.confirm_nightly = !this.confirm_nightly;
                    cx.notify();
                }
            }),
        );
        if on || !self.confirm_nightly {
            return switch_row.into_any_element();
        }
        let warning = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .mx(crate::ui::sp(14.))
            .mb(crate::ui::sp(12.))
            .p(crate::ui::sp(12.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::warn())
            .child(
                div()
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::warn())
                    .font_weight(FontWeight::MEDIUM)
                    .child("Nightly builds can break."),
            )
            .child(
                div()
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_muted())
                    .child(
                        "They are made from main every night and nobody tests them by hand. \
                         One can crash, lose a session or change how your data is stored, and \
                         a release may not read what a nightly wrote. Use them to try what is \
                         next and report what breaks, not for work you can't redo.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(crate::ui::sp(8.))
                    .child(
                        ui::button(
                            "set-nightly-yes",
                            "Get nightly builds",
                            ButtonKind::Danger,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_nightly = false;
                            set(Channel::Nightly, cx);
                        })),
                    )
                    .child(
                        ui::button("set-nightly-no", "Cancel", ButtonKind::Ghost, true).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.confirm_nightly = false;
                                cx.notify();
                            }),
                        ),
                    ),
            );
        div()
            .flex()
            .flex_col()
            .child(switch_row)
            .child(warning)
            .into_any_element()
    }

    /// Puts `id` in use across the app and remembers it.
    /// The Crash reports (`crashes`) or Usage counts switch. It shows what is
    /// sent; when an environment variable decides that, it says which and
    /// does not change.
    fn telemetry_row(&self, crashes: bool, cx: &mut Context<Self>) -> AnyElement {
        use crate::telemetry::{self, Consent};
        let saved = telemetry::consent().unwrap_or_default();
        let locked = telemetry::env_override();
        let shown = if locked.is_some() {
            telemetry::effective()
        } else {
            saved
        };
        let (title, hint, on) = if crashes {
            (
                "Crash reports",
                "Crashes, with their stack trace, and the kind of error when an agent stops. Paths, links, emails and key-like values are removed from a crash's message; its other words are sent.",
                shown.crashes,
            )
        } else {
            (
                "Usage counts",
                "Counts such as sessions started, with the agent's name. Never prompts, replies, files or URLs.",
                shown.usage,
            )
        };
        let hint: SharedString = match locked {
            Some(var) => format!("{hint} Set by {var} in the environment.").into(),
            None => hint.into(),
        };
        row(title, hint.clone(), switch(on).into_any_element())
            .id(if crashes {
                "set-crash-reports"
            } else {
                "set-usage-counts"
            })
            .map(|d| ui::as_switch(d, title, on))
            .aria_description(hint)
            .when(locked.is_none(), |d| d.cursor_pointer())
            .on_click(cx.listener(move |this, _, _, cx| {
                if locked.is_some() {
                    return;
                }
                let choice = if crashes {
                    Consent {
                        crashes: !saved.crashes,
                        ..saved
                    }
                } else {
                    Consent {
                        usage: !saved.usage,
                        ..saved
                    }
                };
                this.telemetry_error = telemetry::set_consent(choice).err().map(Into::into);
                cx.notify();
            }))
            .into_any_element()
    }

    /// What Colors and Shape are set to, and the way to the Customize panel
    /// where they are changed: there the app stays in view, not under a dialog.
    fn render_appearance(&self, cx: &mut Context<Self>) -> AnyElement {
        let value = |text: String| {
            div()
                .flex_none()
                .max_w(px(360.))
                .truncate()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text())
                .child(text)
                .into_any_element()
        };
        let open = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(12.))
            .child(
                ui::button_with(
                    "set-customize",
                    Some("paint-board"),
                    "Open Customize",
                    ButtonKind::Primary,
                    true,
                )
                .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::OpenCustomize))),
            )
            .child(
                div()
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_subtle())
                    .child("Closes Settings and opens the panel at the right."),
            )
            .into_any_element();
        let zoom = {
            let pct = crate::ui_zoom::pct();
            let step = |id: &'static str, icon: &'static str, label: &'static str, steps: i32| {
                let to = crate::ui_zoom::next(pct, steps);
                ui::icon_button(id, icon, 13., label)
                    .when(to != pct, |b| {
                        b.on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(SettingsEvent::Zoom(to));
                            cx.notify();
                        }))
                    })
                    .when(to == pct, |b| b.opacity(0.4))
            };
            row(
                "Zoom",
                "Makes the whole interface bigger or smaller, like a browser's zoom. Ctrl+= and Ctrl+- do it too, Ctrl+0 goes back to 100%.",
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .child(step("set-zoom-out", "minus-sign", "Zoom out", -1))
                    .child(
                        div()
                            .w(px(44.))
                            .text_center()
                            .text_size(px(ui::TEXT_SM))
                            .child(format!("{pct}%")),
                    )
                    .child(step("set-zoom-in", "add01", "Zoom in", 1))
                    .child(
                        ui::button("set-zoom-reset", "Reset", ButtonKind::Ghost, pct != 100).on_click(
                            cx.listener(move |_, _, _, cx| {
                                if pct != 100 {
                                    cx.emit(SettingsEvent::Zoom(100));
                                    cx.notify();
                                }
                            }),
                        ),
                    )
                    .into_any_element(),
            )
            .into_any_element()
        };
        let motion = {
            use crate::ui::motion::{self, Reduce};
            let chosen = motion::choice();
            let mut seg = ui::segmented();
            for r in Reduce::ALL {
                seg = seg.child(
                    ui::segment(
                        ("set-reduce-motion", r as usize),
                        None,
                        r.label(),
                        r == chosen,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SettingsEvent::ReduceMotion(r));
                        cx.notify();
                    })),
                );
            }
            row(
                "Reduce motion",
                "Holds the mascot, spinners and the island's working dots still, and shows menus and dialogs without sliding them in. System follows your computer's setting.",
                seg.into_any_element(),
            )
            .into_any_element()
        };
        stack(vec![
            self.render_tabs_and_sessions(cx),
            group(vec![
                row("Colors", "", value(self.look.colors.clone())).into_any_element(),
                row("Shape", "", value(self.look.shape.clone())).into_any_element(),
                row("Sidebar", "", value(self.look.sidebar.clone())).into_any_element(),
            ]),
            open,
            titled("Window", vec![zoom, motion]),
        ])
    }

    /// Tabs and sessions: three presets on top, then the options they set.
    fn render_tabs_and_sessions(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::tab_layout::{
            BarShows, LongTitles, Opt, Preset, Quiet, SessionsIn, Sort, StateMark, TabLayout,
        };
        let layout = crate::tab_layout::current();
        let chosen = layout.preset();
        let tiles = div()
            .id("tabs-presets")
            .role(gpui::Role::RadioGroup)
            .aria_label("Tabs and sessions preset")
            .flex()
            .gap(crate::ui::sp(12.))
            .children(
                Preset::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(i, p)| preset_tile(i, p, chosen == Some(p), cx)),
            );
        let note = match chosen {
            Some(p) => format!("Set by {}", p.label()),
            None => "Arranged by you".to_string(),
        };
        // A segmented control for one option; off when the other options
        // make it mean nothing.
        let seg = |id: &'static str,
                   on: bool,
                   choices: Vec<(&'static str, bool, bool, TabLayout)>,
                   cx: &mut Context<Self>| {
            let mut s = ui::segmented();
            for (i, (label, picked, allowed, next)) in choices.into_iter().enumerate() {
                let live = on && allowed;
                s = s.child(
                    ui::segment((id, i), None, label, picked)
                        .when(!live, |d| d.opacity(0.4))
                        .when(live, |d| {
                            d.on_click(cx.listener(move |this, _, _, cx| {
                                // The window widens a sidebar too narrow for the tree.
                                if next.tree() && !layout.tree() {
                                    this.rail_w = this.rail_w.max(crate::tab_layout::SIDEBAR_W);
                                }
                                cx.emit(SettingsEvent::TabLayout(next, None));
                                cx.notify();
                            }))
                        }),
                );
            }
            s.into_any_element()
        };
        let with = |f: &dyn Fn(&mut TabLayout)| layout.with(f);
        let sessions_in = seg(
            "tabs-sessions-in",
            true,
            [
                ("Sidebar", SessionsIn::Sidebar),
                ("Title bar", SessionsIn::TitleBar),
                ("Both", SessionsIn::Both),
            ]
            .into_iter()
            .map(|(l, v)| {
                (
                    l,
                    layout.sessions_in == v,
                    true,
                    with(&|t| t.sessions_in = v),
                )
            })
            .collect(),
            cx,
        );
        let bar_shows = seg(
            "tabs-bar-shows",
            true,
            [
                ("Pinned", BarShows::Pinned),
                ("This project", BarShows::Project),
                ("All open", BarShows::AllOpen),
            ]
            .into_iter()
            .map(|(l, v)| {
                (
                    l,
                    layout.bar_shows == v,
                    layout.allows_bar(v),
                    with(&|t| t.bar_shows = v),
                )
            })
            .collect(),
            cx,
        );
        let state = seg(
            "tabs-state",
            true,
            [
                ("Icon", StateMark::Icon),
                ("Dot", StateMark::Dot),
                ("Word", StateMark::Word),
            ]
            .into_iter()
            .map(|(l, v)| (l, layout.state == v, true, with(&|t| t.state = v)))
            .collect(),
            cx,
        );
        let quiet = seg(
            "tabs-quiet",
            layout.applies(Opt::Quiet),
            [
                ("Open", Quiet::Open),
                ("Folded", Quiet::Folded),
                ("Hidden", Quiet::Hidden),
            ]
            .into_iter()
            .map(|(l, v)| (l, layout.quiet == v, true, with(&|t| t.quiet = v)))
            .collect(),
            cx,
        );
        let titles = seg(
            "tabs-titles",
            layout.applies(Opt::Titles),
            [
                ("Cut", LongTitles::Cut),
                ("Two lines", LongTitles::TwoLines),
            ]
            .into_iter()
            .map(|(l, v)| (l, layout.titles == v, true, with(&|t| t.titles = v)))
            .collect(),
            cx,
        );
        let sort = seg(
            "tabs-sort",
            layout.applies(Opt::Sort),
            Sort::ALL
                .into_iter()
                .map(|v| (v.label(), layout.sort == v, true, with(&|t| t.sort = v)))
                .collect(),
            cx,
        );
        let group_on = layout.applies(Opt::Group);
        let grouped = with(&|t| t.group = !layout.group);
        let group_switch = div()
            .id("tabs-group")
            .map(crate::ui::focusable)
            .role(gpui::Role::Switch)
            .aria_label("Group by project")
            .aria_toggled(crate::ui::toggled(layout.group))
            .child(ui::switch(layout.group))
            .when(!group_on, |d| d.opacity(0.4))
            .when(group_on, |d| {
                d.cursor_pointer().on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(SettingsEvent::TabLayout(grouped, None));
                    cx.notify();
                }))
            })
            .into_any_element();
        let width = {
            const MIN: f32 = 160.;
            const MAX: f32 = 320.;
            let w = self.rail_w.clamp(MIN, MAX);
            let this = cx.entity();
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(12.))
                .child(div().w(px(200.)).child(ui::slider(
                    "tabs-sidebar-w",
                    (w - MIN) / (MAX - MIN),
                    ui::blend_track(theme::fill_selected(), theme::text_faint()),
                    theme::text(),
                    move |t, _, cx| {
                        let w = (MIN + t.clamp(0., 1.) * (MAX - MIN)).round();
                        this.update(cx, |d, cx| {
                            if d.rail_w != w {
                                d.rail_w = w;
                                cx.emit(SettingsEvent::SidebarWidth(w));
                                cx.notify();
                            }
                        });
                    },
                )))
                .child(
                    div()
                        .w(px(32.))
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text())
                        .child(format!("{w:.0}")),
                )
                .into_any_element()
        };
        let dim = |on: bool, r: gpui::Div| r.when(!on, |d| d.opacity(0.6)).into_any_element();
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .child(ui::overline("Tabs and sessions").px(crate::ui::sp(4.)))
            .child(
                div()
                    .px(crate::ui::sp(4.))
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_subtle())
                    .child("Start from a preset, then change what you like. Changes show in the window at once."),
            )
            .child(tiles)
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(crate::ui::sp(4.))
                    .pt(crate::ui::sp(8.))
                    .child(div().flex_1().child(ui::overline("Options")))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(note),
                    ),
            )
            .child(group(vec![
                row("Sessions live in", "Where every session of every project is listed", sessions_in)
                    .into_any_element(),
                row("Title bar shows", "Pinned: only what you pull up from the sidebar", bar_shows)
                    .into_any_element(),
                row("State", "How a working, waiting or failed session shows", state)
                    .into_any_element(),
                dim(group_on, row("Group by project", "Each project heads its own sessions", group_switch)),
                dim(
                    layout.applies(Opt::Sort),
                    row("Sort sessions", "In the sidebar tree; Recent puts the last one that moved on top", sort),
                ),
                dim(
                    layout.applies(Opt::Quiet),
                    row("Projects with nothing running", "In the sidebar tree", quiet),
                ),
                dim(
                    layout.applies(Opt::Titles),
                    row("Long titles", "In the sidebar; tabs always cut", titles),
                ),
                row(
                    "Sidebar width",
                    "The tree needs about 240 to show a title and its state",
                    width,
                )
                .into_any_element(),
            ]))
            .into_any_element()
    }

    /// The open project's setup script, check and base branch, saved as they are typed.
    fn render_project(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(project) = &self.project else {
            return group(vec![
                row(
                    "No project open",
                    "Add or open a project, then come back to set its setup script, check command and base branch.",
                    div().into_any_element(),
                )
                .into_any_element(),
            ]);
        };
        if project.remote {
            return stack(vec![titled(
                &project.name,
                vec![
                    row(
                        "On a server",
                        "This project comes from a server, which keeps its own settings. A setup script, a check command and a base branch are set on a project on this computer.",
                        div().into_any_element(),
                    )
                    .into_any_element(),
                ],
            )]);
        }
        stack(vec![titled(
            &project.name,
            vec![
                stacked_row(
                    "Setup script",
                    "Runs in a terminal of every new environment of this project on this computer, right after it is created. Not on servers. Empty turns it off.",
                    field(self.setup_input.clone(), 420.).into_any_element(),
                )
                .into_any_element(),
                stacked_row(
                    "Check command",
                    "Runs in the environment after every agent turn that edits files. When it fails, Arakun sends the output back to the agent, up to 3 times in a row. Agents can run it themselves with arakun check. Empty turns it off.",
                    field(self.check_input.clone(), 420.).into_any_element(),
                )
                .into_any_element(),
                stacked_row(
                    "Base branch",
                    "New environments of this project on this computer start from this branch as it is on origin, fetched first, or from the local branch when origin has none. Empty uses origin's default branch, usually main.",
                    field(self.base_input.clone(), 420.).into_any_element(),
                )
                .into_any_element(),
                stacked_row(
                    "Pass to agents",
                    "Agents don't get Arakun's own variables that look like secrets, such as GH_TOKEN or a cloud key, sandboxed or not. Their own provider keys stay. Name the variables this project's agents need, separated by spaces. Terminals keep them all. Applies to sessions started afterwards.",
                    field(self.pass_input.clone(), 420.).into_any_element(),
                )
                .into_any_element(),
                // Windows has no sandbox to put them in.
                if cfg!(windows) {
                    row(
                        "Sandbox agents",
                        "Windows has no agent sandbox yet. On macOS and Linux this runs the agents of a project in a sandbox.",
                        div().into_any_element(),
                    )
                    .into_any_element()
                } else {
                toggle_row("set-project-sandbox", "Sandbox agents", if cfg!(target_os = "macos") {
                        "Agents of this project run in a Seatbelt sandbox. They can write only the environment, its Git folder (not its config or hooks), build caches and temporary folders, and can't read other tools' logins or files that look like secrets. They keep the network and the keychain. Applies to sessions started afterwards."
                    } else {
                        "Agents of this project run in a bubblewrap sandbox. They can write only the environment, its Git folder (not its config or hooks), build caches and temporary folders, and can't read other tools' logins or files that look like secrets. They keep the network. Applies to sessions started afterwards."
                    }, project.sandbox)
                .on_click(cx.listener(|this, _, _, cx| {
                    let Some(p) = this.project.as_mut().filter(|p| !p.remote) else {
                        return;
                    };
                    p.sandbox = !p.sandbox;
                    cx.emit(SettingsEvent::ProjectSandbox {
                        project: p.path.clone(),
                        on: p.sandbox,
                    });
                    cx.notify();
                }))
                .into_any_element()
                },
            ],
        ), self.render_saved_secrets(project, cx)])
    }

    fn render_voice(&self, cx: &mut Context<Self>) -> AnyElement {
        let enabled = self.voice.enabled;
        let busy = self.voice_busy;
        let key_set = self.voice_key == Some(true);
        let typed_id = self.voice_input.read(cx).text().trim().to_string();
        let id_ok = voice::valid_voice_id(&typed_id);
        let can_test = key_set && id_ok && !busy;

        let key_control = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(6.))
            .child(field(self.key_input.clone(), 260.))
            .child(
                ui::button("set-voice-key-save", "Save", ButtonKind::Outline, !busy)
                    .on_click(cx.listener(|this, _, _, cx| this.save_key(cx))),
            )
            .when(key_set, |d| {
                d.child(
                    ui::button("set-voice-key-remove", "Remove", ButtonKind::Danger, !busy)
                        .on_click(cx.listener(|this, _, _, cx| this.remove_key(cx))),
                )
            });
        let key_hint = match self.voice_key {
            None => "Checking the system keyring…",
            Some(true) => "Saved in the system keyring. Paste a new one and Save to replace it.",
            Some(false) => {
                "From elevenlabs.io › Developers › API Keys. Arakun keeps it in the system keyring."
            }
        };
        let id_hint = if typed_id.is_empty() || id_ok {
            "Add a voice from the Voice Library to My Voices, then copy its ID."
        } else {
            "A voice ID is only letters and digits."
        };

        let tone = self.voice.tone;
        let tones = ui::segmented().children(Tone::ALL.into_iter().map(|t| {
            ui::segment(t.label(), None, t.label(), t == tone)
                .on_click(cx.listener(move |this, _, _, cx| this.set_tone(t, cx)))
        }));

        let test_control = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(6.))
            .when(busy, |d| d.child(ui::spinner(12., theme::mix(0.5))))
            .child(
                ui::button(
                    "set-voice-test",
                    "Test voice",
                    ButtonKind::Outline,
                    can_test,
                )
                .when(can_test, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.test_voice(cx)))
                }),
            );

        let note = self.voice_note.clone().or_else(|| {
            voice::last_error().map(|e| {
                (
                    format!("The last question could not be read. {e}").into(),
                    true,
                )
            })
        });

        let mut blocks = vec![
            titled(
                "Read aloud",
                vec![
                    toggle_row("set-voice", "Read questions aloud", "When Droid asks you something, Arakun says the question and its options, even with the session on screen. It stops once you answer.", enabled)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_voice_enabled(!enabled, cx)))
                    .into_any_element(),
                    row(
                        "Tone",
                        "How much the voice acts out what it reads. Calm sounds the steadiest.",
                        tones.into_any_element(),
                    )
                    .into_any_element(),
                ],
            ),
            titled(
                "ElevenLabs",
                vec![
                    stacked_row("API key", key_hint, key_control.into_any_element()).into_any_element(),
                    stacked_row("Voice ID", id_hint, field(self.voice_input.clone(), 260.).into_any_element())
                        .into_any_element(),
                    row(
                        "Test voice",
                        "Says a short sample with this key and voice.",
                        test_control.into_any_element(),
                    )
                    .into_any_element(),
                ],
            ),
        ];
        if let Some((text, failed)) = note {
            blocks.push(
                div()
                    .px(crate::ui::sp(4.))
                    .text_size(px(ui::TEXT_SM))
                    .line_height(px(17.))
                    .text_color(if failed {
                        theme::danger().into()
                    } else {
                        theme::text_subtle()
                    })
                    .child(text)
                    .into_any_element(),
            );
        }
        stack(blocks)
    }

    fn render_shortcuts(&self, cx: &App) -> AnyElement {
        let q = self.query(cx);
        let mut groups: Vec<(&str, Vec<crate::keymap::Line>)> = Vec::new();
        for line in crate::keymap::sheet()
            .into_iter()
            .filter(|line| Self::shortcut_matches(&q, line))
        {
            match groups.iter_mut().find(|(g, _)| *g == line.group) {
                Some((_, v)) => v.push(line),
                None => groups.push((line.group, vec![line])),
            }
        }
        if groups.is_empty() {
            return div()
                .py(crate::ui::sp(24.))
                .text_center()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .child("No shortcut matches")
                .into_any_element();
        }
        stack(
            groups
                .into_iter()
                .map(|(name, items)| {
                    titled(
                        name,
                        items
                            .into_iter()
                            .map(|line| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(crate::ui::sp(12.))
                                    .min_h(px(36.))
                                    .px(crate::ui::sp(14.))
                                    .py(crate::ui::sp(6.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_size(px(ui::TEXT_MD))
                                            .text_color(theme::text_muted())
                                            .child(line.label),
                                    )
                                    .child(keycaps(&Self::shortcut_keys(&line)))
                                    .into_any_element()
                            })
                            .collect(),
                    )
                })
                .collect(),
        )
    }
}

impl Focusable for SettingsDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

/// A text field sized for a settings row.
fn field(input: Entity<TextInput>, width: f32) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .w(px(width))
        .h(px(ui::BUTTON_H))
        .px(crate::ui::sp(8.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .border_1()
        .border_color(theme::border())
        .bg(theme::background())
        .text_size(px(ui::TEXT_SM))
        .overflow_hidden()
        .child(input)
}

/// Rows in one card, divided by hairlines.
fn group(rows: Vec<AnyElement>) -> AnyElement {
    ui::surface_card()
        .children(rows.into_iter().enumerate().map(|(i, r)| {
            div()
                .when(i > 0, |d| {
                    d.border_t_1().border_color(theme::border_subtle())
                })
                .child(r)
        }))
        .into_any_element()
}

/// A card of rows under its caption: one part of a section.
fn titled(caption: &str, rows: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(8.))
        .child(ui::overline(caption).px(crate::ui::sp(4.)))
        .child(group(rows))
        .into_any_element()
}

/// A preset of Tabs and sessions: a small drawing of the window it makes,
/// its name and what it does.
fn preset_tile(
    i: usize,
    p: crate::tab_layout::Preset,
    chosen: bool,
    cx: &mut Context<SettingsDialog>,
) -> AnyElement {
    use crate::tab_layout::Preset;
    let bar = |n: usize| {
        div()
            .flex()
            .gap(crate::ui::sp(3.))
            .children((0..n).map(|_| {
                div()
                    .w(px(14.))
                    .h(px(4.))
                    .rounded(crate::ui::rad(2.))
                    .bg(theme::text_faint())
            }))
    };
    let lines = |n: usize| {
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(4.))
            .children((0..n).map(|k| {
                div()
                    .h(px(2.))
                    .w(px(if k % 3 == 0 { 34. } else { 26. }))
                    .bg(theme::text_faint())
            }))
    };
    let (tree, left_tabs, right_tabs) = match p {
        Preset::Focused => (5, 2, 0),
        Preset::Classic => (2, 2, 2),
        Preset::Everything => (7, 2, 2),
    };
    let drawing = div()
        .flex()
        .gap(crate::ui::sp(8.))
        .h(px(64.))
        .p(crate::ui::sp(8.))
        .rounded(crate::ui::rad(6.))
        .bg(theme::fill_raised())
        .child(lines(tree))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .child(bar(left_tabs))
                        .child(bar(right_tabs)),
                )
                .child(
                    div()
                        .flex_1()
                        .rounded(crate::ui::rad(3.))
                        .bg(theme::fill_selected()),
                ),
        );
    div()
        .id(("tabs-preset", i))
        .map(crate::ui::focusable)
        .map(|d| ui::labelled(d, gpui::Role::RadioButton, p.label()))
        .aria_toggled(crate::ui::toggled(chosen))
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(8.))
        .p(crate::ui::sp(10.))
        .rounded(crate::ui::rad(ui::RADIUS_LG))
        .border_1()
        .border_color(if chosen {
            theme::active()
        } else {
            theme::border()
        })
        .cursor_pointer()
        .hover(|s| s.bg(theme::fill_hover()))
        .child(drawing)
        .child(
            div()
                .flex()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .text_size(px(ui::TEXT_MD))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text())
                        .child(p.label()),
                )
                .when(chosen, |d| {
                    d.child(icon("tick02", 14.).text_color(theme::active()))
                }),
        )
        .child(
            div()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .child(p.blurb()),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            // The window widens a sidebar too narrow for the tree; say so here too.
            if let Some(min) = p.min_sidebar_w() {
                this.rail_w = this.rail_w.max(min);
            }
            cx.emit(SettingsEvent::TabLayout(p.layout(), Some(p)));
            cx.notify();
        }))
        .into_any_element()
}

/// A section's parts, one under the other.
fn stack(parts: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(24.))
        .children(parts)
        .into_any_element()
}

/// A setting's name, and under it a hint of a line or two.
fn label_block(title: impl Into<SharedString>, hint: impl Into<SharedString>) -> gpui::Div {
    let hint = hint.into();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(crate::ui::sp(2.))
        .child(
            div()
                .text_size(px(ui::TEXT_MD))
                .line_height(px(18.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text())
                .child(title.into()),
        )
        .when(!hint.is_empty(), |d| {
            d.child(
                div()
                    .text_size(px(ui::TEXT_SM))
                    .line_height(px(16.))
                    .text_color(theme::text_subtle())
                    .child(hint),
            )
        })
}

/// Why the agent's section has no sign-in prompt of Arakun's own: it uses the
/// CLI's sign-in, read only. People who see plan limits appear without being
/// asked should know where they come from.
fn no_sign_in_row(agent: AgentKind) -> AnyElement {
    row(
        "No Arakun account",
        crate::agent::cli_login::notice(agent),
        div().into_any_element(),
    )
    .into_any_element()
}

/// One setting: what it is on the left, its control on the right. A control
/// wider than about 200 px goes in a `stacked_row` instead, so it doesn't
/// squeeze the text into a narrow column.
fn row(
    title: impl Into<SharedString>,
    hint: impl Into<SharedString>,
    control: AnyElement,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(16.))
        .px(crate::ui::sp(14.))
        .py(crate::ui::sp(12.))
        .hover(|s| s.bg(theme::fill_raised()))
        .child(label_block(title, hint))
        .child(control)
}

/// One setting that turns on and off: `row` with a switch, clickable as a
/// whole, and read by screen readers as a switch with the hint after it.
fn toggle_row(
    id: impl Into<gpui::ElementId>,
    title: impl Into<SharedString>,
    hint: impl Into<SharedString>,
    on: bool,
) -> gpui::Stateful<gpui::Div> {
    let (title, hint) = (title.into(), hint.into());
    row(title.clone(), hint.clone(), switch(on).into_any_element())
        .id(id)
        .map(|d| ui::as_switch(d, title, on))
        .aria_description(hint)
        .cursor_pointer()
}

/// One setting with its control under the text: chips, a field, or more
/// choices than fit beside it.
fn stacked_row(
    title: impl Into<SharedString>,
    hint: impl Into<SharedString>,
    control: AnyElement,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(10.))
        .px(crate::ui::sp(14.))
        .py(crate::ui::sp(12.))
        .child(label_block(title, hint))
        // In a row of its own, so a segmented control keeps its width
        // instead of stretching across the card.
        .child(div().flex().flex_wrap().items_center().child(control))
}

/// "Ctrl+K / Ctrl+P" as key caps: Ctrl K / Ctrl P, and ⌘ K / ⌘ P on macOS.
fn keycaps(keys: &str) -> AnyElement {
    let keys = crate::platform::shortcut_label(keys);
    let cap = |k: &str| {
        div()
            .min_w(px(20.))
            .h(px(20.))
            .px(crate::ui::sp(4.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(crate::ui::rad(ui::RADIUS_SM))
            .border_1()
            .border_color(theme::border())
            .bg(theme::fill_hover())
            .font_family(theme::FONT_MONO)
            .text_size(px(ui::TEXT_XS))
            .text_color(theme::text_muted())
            .child(k.to_string())
    };
    let sep = |s: &str| {
        div()
            .px(crate::ui::sp(2.))
            .text_size(px(ui::TEXT_XS))
            .text_color(theme::text_faint())
            .child(s.to_string())
    };
    let mut out = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(4.));
    for (i, alt) in keys.split(" / ").enumerate() {
        if i > 0 {
            out = out.child(sep("/"));
        }
        for (j, part) in alt.split(" … ").enumerate() {
            if j > 0 {
                out = out.child(sep("…"));
            }
            let combo: Vec<&str> = if part.ends_with("++") {
                vec![part.trim_end_matches("++"), "+"]
            } else if !part.contains('+') {
                // macOS writes "⇧⌘T": one cap per modifier symbol, then the key.
                let key = part.trim_start_matches(['⌃', '⌥', '⇧', '⌘']);
                let mods = &part[..part.len() - key.len()];
                mods.char_indices()
                    .map(|(i, c)| &mods[i..i + c.len_utf8()])
                    .chain([key])
                    .collect()
            } else {
                part.split('+').collect()
            };
            for k in combo {
                out = out.child(cap(k));
            }
        }
    }
    out.into_any_element()
}

pub use crate::ui::switch;

impl Render for SettingsDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let q = self.query(cx);
        let shown = self.shown(cx);

        // Filled and borderless, like the rail's search, so it reads as part
        // of the list it filters.
        let search = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(8.))
            .h(px(ui::INPUT_H))
            .px(crate::ui::sp(10.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .bg(theme::fill_hover())
            .text_size(px(ui::TEXT_MD))
            .line_height(px(20.))
            .child(icon("search01", 14.).text_color(theme::text_subtle()))
            .child(self.input.clone());

        let mut list = div()
            .id("settings-sections")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pt(crate::ui::sp(16.))
            .gap(crate::ui::sp(2.));
        for (i, group) in Group::ALL.into_iter().enumerate() {
            let rows: Vec<_> = shown.iter().filter(|s| s.group() == group).collect();
            if rows.is_empty() {
                continue;
            }
            list = list.child(
                ui::overline(group.title())
                    .px(crate::ui::sp(8.))
                    .pb(crate::ui::sp(6.))
                    .when(i > 0, |d| d.pt(crate::ui::sp(16.))),
            );
            for s in rows {
                list = list.child(self.render_row(*s, cx));
            }
        }

        // The version, or how many sections the search leaves.
        let foot: SharedString = if q.is_empty() {
            format!("Arakun {}", env!("ARAKUN_VERSION")).into()
        } else {
            match shown.len() {
                0 => "No matches".into(),
                1 => "1 section".into(),
                n => format!("{n} sections").into(),
            }
        };

        let nav = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(LIST_W))
            .h_full()
            .px(crate::ui::sp(8.))
            .py(crate::ui::sp(12.))
            .border_r_1()
            .border_color(theme::border_subtle())
            .child(search)
            .child(list)
            .child(
                div()
                    .flex_none()
                    .px(crate::ui::sp(8.))
                    .pt(crate::ui::sp(8.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child(foot),
            );

        let pane: AnyElement = if shown.is_empty() {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(crate::ui::sp(10.))
                .h_full()
                .px(crate::ui::sp(24.))
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .child(icon("search01", 20.).text_color(theme::text_faint()))
                .child(crate::platform::shortcut_label(
                    "Nothing matches. Try “diff”, “glass” or a key like “Ctrl+T”.",
                ))
                .into_any_element()
        } else {
            self.render_pane(cx)
        };

        ui::modal_panel("settings", DIALOG_W, "Settings")
            .child(ui::modal_header(
                "Settings",
                Some("Changes save as you make them.".into()),
                ui::close_button("settings-close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Dismissed))),
            ))
            .child(
                div()
                    .flex()
                    .h(px(BODY_H))
                    .min_h_0()
                    .mt(crate::ui::sp(12.))
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .overflow_hidden()
                    .child(nav)
                    .child(pane),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The link setting is in General in every build, whichever keyword list it has.
    #[test]
    fn searching_for_links_finds_general() {
        assert!(Section::General.matches("open links"));
        assert!(Section::General.matches("external browser"));
    }

    #[test]
    fn pass_to_agents_takes_names_once() {
        assert_eq!(
            pass_env_names(" GH_TOKEN, NPM_TOKEN\tGH_TOKEN,,"),
            ["GH_TOKEN", "NPM_TOKEN"]
        );
        assert!(pass_env_names("  ").is_empty());
        assert!(Section::Project.matches("gh_token"));
    }

    #[test]
    fn searching_for_merge_finds_branches() {
        assert!(Section::Branches.matches("archive after merge"));
        assert!(Section::Branches.matches("settle threads"));
        assert!(Section::Branches.matches("after merge"));
    }

    #[test]
    fn shortcut_search_reads_keys_and_words() {
        let sc = &crate::keymap::Line {
            group: "Tabs and panes",
            label: "Close the tab",
            keys: vec!["ctrl-w"],
        };
        assert!(SettingsDialog::shortcut_matches("ctrl+w", sc));
        assert!(SettingsDialog::shortcut_matches("close", sc));
        assert!(SettingsDialog::shortcut_matches("panes", sc));
        assert!(!SettingsDialog::shortcut_matches("terminal", sc));
    }

    /// ↑ and ↓ walk `Section::ALL`, and the list draws it group by group, so
    /// the two orders must agree or the keys would jump around the list.
    #[test]
    fn sections_are_listed_group_by_group() {
        let groups: Vec<Group> = Section::ALL.iter().map(|s| s.group()).collect();
        let mut sorted = groups.clone();
        sorted.sort_by_key(|g| Group::ALL.iter().position(|x| x == g));
        assert_eq!(groups, sorted);
        assert!(
            Section::ALL
                .iter()
                .all(|s| (s.group() == Group::Agents) == s.agent().is_some())
        );
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
        assert!(found("branch").contains(&Section::Branches));
        assert!(found("haiku").contains(&Section::Branches));
        assert!(found("crash").contains(&Section::Privacy));
        assert!(found("update").contains(&Section::General));
        assert!(found("automatic updates").contains(&Section::General));
        assert!(found("redacted").contains(&Section::Privacy));
        assert!(found("stream").contains(&Section::Privacy));
        assert!(found("secrets").contains(&Section::Privacy));
        assert!(!found("crash").contains(&Section::General));
        assert!(found("factory").contains(&Section::Droid));
        assert!(found("github").contains(&Section::Account));
        assert!(found("teams").contains(&Section::Account));
        assert!(found("ctrl+w").contains(&Section::Shortcuts));
        assert!(found("zoom").contains(&Section::Appearance));
        assert!(found("check command").contains(&Section::Project));
        assert!(found("setup script").contains(&Section::Project));
        assert!(found("saved secrets").contains(&Section::Project));
        assert!(found("replace").contains(&Section::Project));
        assert!(found("github").contains(&Section::General));
        assert_eq!(found("").len(), Section::ALL.len());
        assert!(found("zzz-nothing").is_empty());
    }
}
