//! The Settings dialog: the search and the sections on the left, grouped as
//! Arakun's own and the agents', and the chosen section on the right as
//! captioned cards of rows.

use crate::ui::Typography as _;
use std::sync::atomic::{AtomicU8, Ordering};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};

use crate::agent::claude_auth;
use crate::agent::plan::{self, PlanUsage};
use crate::env_name;
use crate::input::{InputEvent, TextInput};
use crate::theme::{self, DiffPalette};
use crate::ui::{self, ButtonKind, icon};
use crate::voice::{self, Lang, Tone};

pub enum SettingsEvent {
    Dismissed,
    ClaudeSignIn,
    ClaudeSignOut,
    OpenCodeSignIn,
    OpenCodeGoSignIn,
    OpenCodeSignOut,
    CodexSignIn,
    CodexSignOut,
    OpenUsage,
    OpenGallery,
    RailUsage(crate::agent::AgentKind, bool),
    Grays(crate::theme::Grays),
    TypeScale(crate::ui::TypeScale),
    Arcade(bool),
    DiffPalette(DiffPalette),
    Glass(bool),
    Island(bool),
    OpenAccount,
    OpenExtensions,
    #[cfg(feature = "coderlabs")]
    OpenTasks,
    #[cfg(feature = "coderlabs")]
    DisconnectCoderSpace,
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
    Branches,
    Voice,
    Privacy,
    #[cfg(feature = "coderlabs")]
    CoderSpace,
    Shortcuts,
    Claude,
    Codex,
    Droid,
    OpenCode,
}

impl Section {
    /// In the order the list shows them, so ↑ and ↓ move down the list.
    const ALL: &[Section] = &[
        Section::General,
        Section::Appearance,
        Section::Branches,
        Section::Voice,
        Section::Privacy,
        #[cfg(feature = "coderlabs")]
        Section::CoderSpace,
        Section::Shortcuts,
        Section::Claude,
        Section::Codex,
        Section::Droid,
        Section::OpenCode,
    ];

    fn group(self) -> Group {
        if self.agent().is_some() {
            Group::Agents
        } else {
            Group::Arakun
        }
    }

    /// The agent a section is about; the list shows its logo.
    fn agent(self) -> Option<crate::agent::AgentKind> {
        use crate::agent::AgentKind;
        match self {
            Section::Claude => Some(AgentKind::Claude),
            Section::Codex => Some(AgentKind::Codex),
            Section::Droid => Some(AgentKind::Droid),
            Section::OpenCode => Some(AgentKind::OpenCode),
            _ => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Section::General => "General",
            Section::Appearance => "Appearance",
            Section::Branches => "Branches",
            Section::Voice => "Voice",
            Section::Privacy => "Privacy",
            #[cfg(feature = "coderlabs")]
            Section::CoderSpace => "CoderSpace",
            Section::Shortcuts => "Keyboard shortcuts",
            Section::Claude => "Claude Code",
            Section::Codex => "Codex",
            Section::Droid => "Droid",
            Section::OpenCode => "OpenCode",
        }
    }

    /// The list's icon for a section that is not an agent's.
    fn icon(self) -> &'static str {
        match self {
            Section::General => "settings01",
            Section::Appearance => "paint-board",
            Section::Branches => "git-branch",
            Section::Voice => "volume-high",
            Section::Privacy => "shield-alert",
            #[cfg(feature = "coderlabs")]
            Section::CoderSpace => "check-list",
            Section::Shortcuts => "keyboard",
            Section::Claude | Section::Codex | Section::Droid | Section::OpenCode => "bot",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Section::General => "How Arakun behaves when nothing is going on.",
            Section::Appearance => "Colors and surfaces. Changes apply right away.",
            Section::Branches => {
                "A new environment starts on a random branch name; an agent can rename it after the task."
            }
            Section::Voice => "Droid's questions, read aloud with an ElevenLabs voice.",
            Section::Privacy => {
                "What shows on screen, and what Arakun sends to its PostHog under a random id."
            }
            #[cfg(feature = "coderlabs")]
            Section::CoderSpace => "Your pending tasks, inside Arakun.",
            Section::Shortcuts => "Everything you can do without the mouse.",
            Section::Claude => "Claude Code's sign-in and what your plan has left.",
            Section::Codex => "Codex's sign-in: your ChatGPT plan or an OpenAI API key.",
            Section::Droid => "Your Factory plan, and what Droid loads in the visible environment.",
            Section::OpenCode => "The providers OpenCode signs in to, OpenCode Go among them.",
        }
    }

    /// Words the search matches besides the title.
    fn keywords(self) -> &'static str {
        match self {
            #[cfg(feature = "coderlabs")]
            Section::General => {
                "raccoon mascot animation motion new session dynamic island notch top bar agents alerts oye arakun wake word voice microphone"
            }
            #[cfg(not(feature = "coderlabs"))]
            Section::General => {
                "raccoon mascot animation motion new session dynamic island notch top bar agents alerts sidebar usage card plans browser web page drive click type scripts"
            }
            Section::Appearance => {
                "theme palette color colors accent tint corners radius rounded round sharp spacing gap padding compact roomy density window custom share night graphite midnight forest rose diff colors colorblind high contrast glass blur transparent translucent grays gray text size font scale design system mode dark light"
            }
            Section::Branches => "branch name naming environment worktree rename model haiku agent",
            Section::Voice => {
                "elevenlabs speech speak tts read aloud audio sound api key questions tone"
            }
            Section::Privacy => {
                "telemetry analytics usage counts crash reports privacy posthog data redacted redact hide streamer stream demo screen share sharing incognito"
            }
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
        }
    }

    /// Whether the search `query` (lower case, trimmed) leaves this section.
    fn matches(self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let text = format!(
            "{} {} {}",
            self.title(),
            self.description(),
            self.keywords()
        )
        .to_lowercase();
        query.split_whitespace().all(|w| text.contains(w))
            || (self == Section::Shortcuts
                && SHORTCUTS
                    .iter()
                    .any(|sc| SettingsDialog::shortcut_matches(query, sc)))
    }
}

/// The section the last dialog was left on.
static LAST_SECTION: AtomicU8 = AtomicU8::new(0);

/// (group, keys, description)
type Shortcut = (&'static str, &'static str, &'static str);

const SHORTCUTS: &[Shortcut] = &[
    (
        "Navigate",
        "Ctrl+K / Ctrl+P",
        "Jump to a session, environment or command",
    ),
    ("Navigate", "Alt+↑ / Alt+↓", "Previous / next environment"),
    (
        "Navigate",
        "Ctrl+B",
        "Hide or show the sidebar and workspace panel",
    ),
    ("Navigate", "Ctrl+,", "Settings"),
    (
        "Navigate",
        "Ctrl+= / Ctrl+- / Ctrl+0",
        "Zoom the interface in / out / back to 100%",
    ),
    (
        "Navigate",
        "Ctrl+Shift+H",
        "Redacted mode: hide emails, keys and codes on screen",
    ),
    ("Sessions", "Ctrl+T", "New session"),
    ("Sessions", "Esc", "Stop the running agent turn"),
    ("Sessions", "Ctrl+S", "Save the draft as a ! prompt"),
    ("Projects", "Ctrl+O", "Add project"),
    ("Projects", "Ctrl+Shift+N", "New environment"),
    ("Tabs and panes", "Ctrl+W", "Close tab"),
    (
        "Tabs and panes",
        "Alt+← / Alt+→",
        "Back / forward through tabs",
    ),
    (
        "Tabs and panes",
        "Ctrl+Tab / Ctrl+Shift+Tab",
        "Next / previous tab (also Ctrl+PgDn / PgUp)",
    ),
    (
        "Tabs and panes",
        "Ctrl+1 … Ctrl+9",
        "Go to tab 1–8, or the last tab",
    ),
    (
        "Tabs and panes",
        "Ctrl+D / Ctrl+Shift+D",
        "Split right / down (again to unsplit)",
    ),
    (
        "Tabs and panes",
        "Ctrl+Alt+Arrows",
        "Move focus to the other pane",
    ),
    (
        "Terminal and browser",
        "Ctrl+J / Ctrl+`",
        "Toggle terminal dock",
    ),
    ("Terminal and browser", "Ctrl+Shift+T", "New terminal"),
    (
        "Terminal and browser",
        "Ctrl+Shift+F",
        "Find in the terminal (Enter older, ↓ newer)",
    ),
    ("Terminal and browser", "Ctrl+Shift+B", "New browser tab"),
];

/// The queue key of the "Test voice" sample.
const TEST_KEY: &str = "settings-test";
/// How long a palette edit waits for the next before it is saved.
const PALETTE_SAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

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
    /// The palette in use and the user's own.
    palettes: crate::palettes::Saved,
    /// Where a shared palette's line is pasted.
    share_input: Entity<TextInput>,
    /// What the last paste or copy did, and whether it failed.
    palette_note: Option<(SharedString, bool)>,
    /// Delete was clicked once on the palette in use.
    confirm_delete: bool,
    /// The name of the user's palette in use.
    name_input: Entity<TextInput>,
    /// Whether the field to paste a shared palette is open.
    show_import: bool,
    /// The save of a palette edit that waits for the slider to stop.
    palette_save: Option<Task<()>>,
    /// A palette edit is shown but not saved yet.
    palette_dirty: bool,
    /// The save being written off the UI thread, if one is.
    palette_writing: Option<Task<()>>,
    /// The settings pane's scroll, so a slider that moves the layout can stay
    /// under the pointer.
    pane_scroll: gpui::ScrollHandle,
    /// Who names the branch of a new environment after its first prompt.
    namer: env_name::Namer,
    namer_input: Entity<TextInput>,
    /// Why the last crash report or usage count choice could not be saved.
    telemetry_error: Option<SharedString>,
    _subs: [Subscription; 8],
}

impl EventEmitter<SettingsEvent> for SettingsDialog {}

impl SettingsDialog {
    pub fn new(
        arcade: bool,
        droid: bool,
        #[cfg(feature = "coderlabs")] coderspace: Option<String>,
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
        let share_input =
            cx.new(|cx| TextInput::new("Paste a palette: arakun:palette/v1:…", cx).mono());
        let share_sub =
            cx.subscribe_in(
                &share_input,
                window,
                |this: &mut Self, _, ev, _, cx| match ev {
                    InputEvent::Submit => this.import_palette(cx),
                    InputEvent::Cancel => cx.emit(SettingsEvent::Dismissed),
                    _ => {}
                },
            );
        let palettes = crate::palettes::load();
        let name_input = cx.new(|cx| {
            let mut i = TextInput::new("Name this palette", cx);
            i.set_text(palettes.active().name, cx);
            i
        });
        let name_sub =
            cx.subscribe_in(
                &name_input,
                window,
                |this: &mut Self, input, ev, _, cx| match ev {
                    InputEvent::Changed => {
                        let name = input.read(cx).text().trim().to_string();
                        if !name.is_empty() && name != this.palettes.active().name {
                            this.edit_palette(|p| p.name = name, cx);
                        }
                    }
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
        cx.spawn(async move |this, cx| {
            let (plan, access) = cx
                .background_spawn(async move { (plan::refresh(), claude_auth::access()) })
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
                .background_spawn(async move { voice::api_key().is_some() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.voice_key = Some(set);
                cx.notify();
            });
        })
        .detach();

        Self {
            arcade,
            droid,
            #[cfg(feature = "coderlabs")]
            coderspace,
            claude: None,
            claude_access: None,
            opencode: crate::agent::opencode_providers(),
            codex: crate::agent::codex_sign_in(),
            codex_env: crate::agent::codex_env_key(),
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
            palettes,
            share_input,
            palette_note: None,
            confirm_delete: false,
            name_input,
            show_import: false,
            palette_save: None,
            palette_dirty: false,
            palette_writing: None,
            pane_scroll: gpui::ScrollHandle::new(),
            _subs: [
                sub,
                key_sub,
                voice_sub,
                share_sub,
                name_sub,
                namer_sub,
                // An edit made just before the dialog closes or the app quits
                // is saved then rather than lost with its wait.
                cx.on_release(|this, _| this.flush_palette()),
                cx.on_app_quit(|this, _| {
                    this.flush_palette();
                    async {}
                }),
            ],
        }
    }

    fn set_voice_enabled(&mut self, on: bool, cx: &mut Context<Self>) {
        self.voice.enabled = on;
        voice::store(&self.voice);
        if !on {
            voice::stop();
        }
        cx.notify();
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
                .background_spawn(async move { voice::store_api_key(&key) })
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
            cx.background_spawn(async move { voice::clear_api_key() })
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

    fn test_voice(&mut self, lang: Lang, cx: &mut Context<Self>) {
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
                .background_spawn(async move {
                    let key = voice::api_key()
                        .ok_or_else(|| "Save your ElevenLabs API key first.".to_string())?;
                    voice::fetch(&key, &id, tone, voice::sample(lang), lang)
                })
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

    fn shortcut_matches(query: &str, (group, keys, what): &Shortcut) -> bool {
        query.is_empty()
            || what.to_lowercase().contains(query)
            || keys.to_lowercase().contains(query)
            || crate::platform::shortcut_label(keys)
                .to_lowercase()
                .contains(query)
            || group.to_lowercase().contains(query)
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

    /// Whether an agent's section gets the green dot: it can run, through a
    /// sign-in or a key. `false` for the other sections.
    fn signed_in(&self, s: Section) -> bool {
        match s {
            Section::Claude => self.claude.is_some() || self.claude_access.is_some(),
            Section::Codex => self.codex.is_some() || self.codex_env.is_some(),
            Section::Droid => self.droid,
            Section::OpenCode => !self.opencode.is_empty(),
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
                    row(
                        "Raccoon animation",
                        "Follows the cursor and acts out what agents do. Off, it holds still.",
                        switch(self.arcade).into_any_element(),
                    )
                    .id("set-arcade")
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.arcade = !this.arcade;
                        cx.emit(SettingsEvent::Arcade(this.arcade));
                        cx.notify();
                    }))
                    .into_any_element(),
                    row(
                        "Dynamic Island",
                        "What every agent is doing, at the top of the screen, and a card when one finishes, needs you or fails. On Linux it needs Wayland.",
                        switch(crate::island::enabled()).into_any_element(),
                    )
                    .id("set-island")
                    .cursor_pointer()
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(SettingsEvent::Island(!crate::island::enabled()));
                        cx.notify();
                    }))
                    .into_any_element(),
                    #[cfg(feature = "coderlabs")]
                    row(
                        "Oye Arakun",
                        "The island listens for \"Oye Arakun\" to start a call, on this computer. Off, it doesn't use the microphone until you call it yourself.",
                        switch(crate::island_host::wake::enabled()).into_any_element(),
                    )
                    .id("set-wake")
                    .cursor_pointer()
                    .on_click(cx.listener(|_, _, _, cx| {
                        crate::island_host::wake::set_enabled(!crate::island_host::wake::enabled());
                        cx.notify();
                    }))
                    .into_any_element(),
                ];
                let mut chips = div().flex().flex_wrap().items_center().gap(crate::ui::sp(6.));
                for (i, agent) in crate::agent::AgentKind::ALL.into_iter().enumerate() {
                    let on = crate::account_view::rail_shows(agent);
                    chips = chips.child(
                        ui::chip(("set-rail-usage", i), agent.label(), on).on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(SettingsEvent::RailUsage(agent, !on));
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
                                "Usage card",
                                "The plans the card at the bottom of the sidebar shows; OpenCode's once a Go key is connected. The Usage tab always shows them all.",
                                chips.into_any_element(),
                            )
                            .into_any_element(),
                        ],
                    ),
                    titled(
                        "Browser",
                        vec![
                            row(
                                "Agents drive any web page",
                                "Click, type and run scripts in any tab, with your signed-in accounts. Off, they drive local pages and only read the rest.",
                                switch(crate::shell::pages::drive_any_page()).into_any_element(),
                            )
                            .id("set-drive-pages")
                            .cursor_pointer()
                            .on_click(cx.listener(|_, _, _, cx| {
                                crate::shell::pages::set_drive_any_page(!crate::shell::pages::drive_any_page());
                                cx.notify();
                            }))
                            .into_any_element(),
                        ],
                    ),
                ])
            }
            Section::Appearance => self.render_appearance(cx),
            Section::Branches => {
                let current = self.namer.choice;
                let mut choices = ui::segmented();
                for (i, choice) in env_name::Choice::ALL.into_iter().enumerate() {
                    choices = choices.child(
                        ui::segment(("set-namer", i), None, choice.label(), choice == current)
                            .on_click(cx.listener(move |this, _, _, cx| this.set_namer_choice(choice, cx))),
                    );
                }
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
                )])
            }
            Section::Privacy => {
                let redacted = row(
                    "Redacted mode",
                    "For streams, demos and screen sharing: hides account emails, keys and tokens, pairing codes, addresses and your user name, in the views, the terminals and the agents' replies. Ctrl+Shift+H turns it on and off.",
                    switch(crate::redact::on()).into_any_element(),
                )
                .id("set-redacted")
                .cursor_pointer()
                .on_click(cx.listener(|_, _, _, cx| {
                    if let Err(e) = crate::redact::set(!crate::redact::on()) {
                        eprintln!("arakun: redacted mode will not last past a restart: {e}");
                    }
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
                stack(vec![
                    titled("On screen", vec![redacted]),
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
                    ],
                )])
            }
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
                    ],
                )])
            }
            Section::Voice => self.render_voice(cx),
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
        row(title, hint, switch(on).into_any_element())
            .id(if crashes {
                "set-crash-reports"
            } else {
                "set-usage-counts"
            })
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

    fn use_palette(&mut self, id: &str, cx: &mut Context<Self>) {
        self.palettes.current = id.to_string();
        self.confirm_delete = false;
        self.apply_palette(cx);
    }

    fn apply_palette(&mut self, cx: &mut Context<Self>) {
        self.palette_save = None;
        self.palette_dirty = true;
        self.write_palette(cx);
        self.show_palette(cx);
    }

    /// Shows the palette in use across the app without saving it.
    fn show_palette(&mut self, cx: &mut Context<Self>) {
        crate::palettes::apply(&self.palettes);
        // The name field follows the palette in use, unless it is what changed.
        let name = self.palettes.active().name;
        if self.name_input.read(cx).text().trim() != name {
            self.name_input.update(cx, |i, cx| i.set_text(name, cx));
        }
        cx.refresh_windows();
        cx.notify();
    }

    /// Changes the palette in use, which is one of the user's (the editor
    /// shows only for those).
    fn edit_palette(&mut self, change: impl FnOnce(&mut theme::Palette), cx: &mut Context<Self>) {
        let mut p = self.palettes.active();
        if !self.palettes.is_custom(&p.id) {
            return;
        }
        change(&mut p);
        self.palettes.update(p);
        self.save_palette_later(cx);
    }

    /// Shows a change now and saves it once changes settle: a slider drag or
    /// typing a name changes the palette many times a second, and each save
    /// is a synced write.
    fn save_palette_later(&mut self, cx: &mut Context<Self>) {
        self.show_palette(cx);
        self.palette_dirty = true;
        self.palette_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PALETTE_SAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| this.write_palette(cx));
        }));
    }

    /// Saves the palettes off the UI thread: each save is a synced write,
    /// which can take long enough to stall typing and drawing. One save at a
    /// time; an edit made while it writes is saved when it is done. A failed
    /// save stays dirty, so closing Settings or quitting tries again.
    fn write_palette(&mut self, cx: &mut Context<Self>) {
        if !self.palette_dirty || self.palette_writing.is_some() {
            return;
        }
        self.palette_dirty = false;
        let (saved, version) = (self.palettes.clone(), crate::palettes::next_version());
        self.palette_writing = Some(cx.spawn(async move |this, cx| {
            let ok = cx
                .background_executor()
                .spawn(async move { crate::palettes::store_version(&saved, version) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.palette_writing = None;
                if !ok {
                    this.palette_dirty = true;
                } else if this.palette_dirty {
                    this.write_palette(cx);
                }
            });
        }));
    }

    /// Saves a palette edit that is still waiting, if there is one, right
    /// away: Settings is closing or the app is quitting, and a save left to
    /// a task would be dropped with it.
    fn flush_palette(&mut self) {
        // A save still writing may not finish once its task is dropped.
        if self.palette_dirty || self.palette_writing.take().is_some() {
            let version = crate::palettes::next_version();
            self.palette_dirty = !crate::palettes::store_version(&self.palettes, version);
        }
    }

    /// A copy of the palette in use that the user can change.
    fn customize_palette(&mut self, cx: &mut Context<Self>) {
        let from = self.palettes.active();
        self.palettes.add(&from);
        self.palette_note = None;
        self.apply_palette(cx);
    }

    fn import_palette(&mut self, cx: &mut Context<Self>) {
        let code = self.share_input.read(cx).text().to_string();
        match theme::Palette::from_share_code(&code) {
            Some(p) => {
                self.palettes.add(&p);
                self.share_input.update(cx, |i, cx| i.set_text("", cx));
                self.show_import = false;
                // A Delete armed for the old palette must not delete the new one.
                self.confirm_delete = false;
                self.palette_note = Some(("Added and in use.".into(), false));
                self.apply_palette(cx);
            }
            None => {
                self.palette_note = Some((
                    "That isn't a palette line. It starts with arakun:palette/v1:".into(),
                    true,
                ));
                cx.notify();
            }
        }
    }

    fn render_palettes(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.palettes.active();
        let all = self.palettes.all();
        let card = |i: usize, p: &theme::Palette, cx: &mut Context<Self>| {
            let on = p.id == active.id;
            let id = p.id.clone();
            let bar =
                |w: f32, c: gpui::Hsla| div().h(px(4.)).w(gpui::relative(w)).rounded_full().bg(c);
            // 6 px (padding and border) inside the card's 12 px corner; the
            // strip on its left rounds the same, as nothing clips it.
            let preview = div()
                .flex()
                .h(px(54.))
                .rounded(ui::nested(ui::RADIUS_LG, 6.))
                .overflow_hidden()
                .bg(p.background())
                .child(
                    div()
                        .w(gpui::relative(0.34))
                        .h_full()
                        .flex()
                        .flex_col()
                        .gap(crate::ui::sp(5.))
                        .p(crate::ui::sp(7.))
                        .rounded_l(ui::nested(ui::RADIUS_LG, 6.))
                        .bg(p.surface(0.07))
                        .child(bar(0.6, p.accent()))
                        .child(bar(0.85, p.content().opacity(0.22)))
                        .child(bar(0.7, p.content().opacity(0.22))),
                )
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(crate::ui::sp(5.))
                        .p(crate::ui::sp(8.))
                        .child(bar(0.8, p.content().opacity(0.8)))
                        .child(bar(0.55, p.content().opacity(0.25)))
                        .child(
                            div()
                                .mt(px(2.))
                                .flex()
                                .items_center()
                                .gap(crate::ui::sp(5.))
                                .p(crate::ui::sp(5.))
                                .rounded(crate::ui::rad(ui::RADIUS_SM))
                                .bg(p.surface(0.12))
                                .child(div().size(px(7.)).rounded_full().bg(p.accent()))
                                .child(bar(0.6, p.content().opacity(0.25))),
                        ),
                );
            div()
                .id(("palette", i))
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .p(crate::ui::sp(5.))
                .rounded(crate::ui::rad(ui::RADIUS_LG))
                .border_1()
                .border_color(if on {
                    theme::active().opacity(0.55)
                } else {
                    theme::stroke()
                })
                .cursor_pointer()
                .hover(|s| s.bg(theme::fill_hover()))
                .child(preview)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .px(crate::ui::sp(3.))
                        .pb(crate::ui::sp(1.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(ui::TEXT_SM))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if on {
                                    theme::text()
                                } else {
                                    theme::text_muted()
                                })
                                .child(p.name.clone()),
                        )
                        .when(on, |d| {
                            d.child(icon("tick02", 13.).text_color(theme::active()))
                        }),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.use_palette(&id, cx)))
        };
        let mut grid = div().flex().flex_col().gap(crate::ui::sp(8.));
        let cards: Vec<_> = all
            .iter()
            .enumerate()
            .map(|(i, p)| card(i, p, cx).into_any_element())
            .collect();
        let mut cards = cards.into_iter();
        loop {
            let row: Vec<_> = cards.by_ref().take(3).collect();
            if row.is_empty() {
                break;
            }
            let pad = 3 - row.len();
            grid = grid.child(
                div()
                    .flex()
                    .gap(crate::ui::sp(8.))
                    .children(row)
                    .children((0..pad).map(|_| div().flex_1())),
            );
        }
        let custom = self.palettes.is_custom(&active.id);
        let head = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(12.))
            .child(label_block(
                "Palette",
                "The colors of the whole app. Customize one to make it yours.",
            ))
            .child(
                ui::button("palette-paste", "Paste a palette", ButtonKind::Ghost, true).on_click(
                    cx.listener(|this, _, window, cx| {
                        this.show_import = !this.show_import;
                        this.palette_note = None;
                        if this.show_import {
                            window.focus(&this.share_input.focus_handle(cx), cx);
                        }
                        cx.notify();
                    }),
                ),
            )
            .when(!custom, |d| {
                d.child(
                    ui::button("palette-customize", "Customize", ButtonKind::Outline, true)
                        .on_click(cx.listener(|this, _, _, cx| this.customize_palette(cx))),
                )
            });
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(12.))
            .px(crate::ui::sp(14.))
            .py(crate::ui::sp(12.))
            .child(head)
            .when(self.show_import, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .child(field(self.share_input.clone(), 0.).flex_1())
                        .child(
                            ui::button("palette-import", "Add", ButtonKind::Outline, true)
                                .on_click(cx.listener(|this, _, _, cx| this.import_palette(cx))),
                        ),
                )
            })
            .when_some(self.palette_note.clone(), |d, (note, bad)| {
                d.child(
                    div()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(if bad {
                            theme::error().into()
                        } else {
                            theme::text_subtle()
                        })
                        .child(note),
                )
            })
            .child(grid)
            .when(custom, |d| d.child(self.render_palette_editor(&active, cx)))
            .into_any_element()
    }

    fn render_palette_editor(&self, p: &theme::Palette, cx: &mut Context<Self>) -> AnyElement {
        const ACCENTS: [u32; 8] = [
            theme::AMBER,
            0xff7a59,
            0xff8fab,
            0xb394f5,
            0x8b9dff,
            0x86a6f2,
            0x5fd4c4,
            0x5fd38d,
        ];
        let this = cx.entity().downgrade();
        let setter = move |f: fn(&mut theme::Palette, f32)| {
            let this = this.clone();
            move |t: f32, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.edit_palette(|p| f(p, t), cx));
            }
        };
        let line = |title: &'static str, value: String, control: AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_muted())
                        .child(title)
                        .child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_subtle())
                                .child(value),
                        ),
                )
                .child(control)
        };
        let dark = theme::Palette {
            bg: 0.03,
            ..p.clone()
        };
        let light = theme::Palette {
            bg: 0.16,
            ..p.clone()
        };
        let gray = theme::Palette {
            tint: 0.,
            ..p.clone()
        };
        let full = theme::Palette {
            tint: 0.6,
            ..p.clone()
        };
        // Colors that wouldn't read are adjusted when drawn; say so, since
        // the slider or swatch then shows something a little different.
        let text_ok = p.readable_bg() >= p.bg;
        let accent_ok = p.readable_accent() == p.accent;
        let readable: (&str, gpui::Hsla) = match (text_ok, accent_ok) {
            (true, true) => ("Text and accent read well", theme::success().into()),
            (false, true) => (
                "The background is drawn darker so text stays readable",
                theme::attention().into(),
            ),
            (true, false) => (
                "The accent is drawn lighter so it stays readable",
                theme::attention().into(),
            ),
            (false, false) => (
                "Drawn with a darker background and a lighter accent so text stays readable",
                theme::attention().into(),
            ),
        };
        let copied = p.share_code();
        let id = p.id.clone();
        let armed = self.confirm_delete;
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(14.))
            .p(crate::ui::sp(14.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .bg(theme::fill_raised())
            .border_1()
            .border_color(theme::stroke())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(6.))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::text_muted())
                            .child("Name"),
                    )
                    .child(field(self.name_input.clone(), 0.).w_full()),
            )
            .child(line(
                "Tint",
                format!("{}°", p.hue.round()),
                ui::slider(
                    "pal-hue",
                    p.hue / 360.,
                    ui::hue_track(0.6, 0.55),
                    gpui::hsla(p.hue / 360., 0.6, 0.55, 1.),
                    setter(|p, t| p.hue = hue_at(t)),
                )
                .into_any_element(),
            ))
            .child(line(
                "Background",
                format!("{:.1}%", p.bg * 100.),
                ui::slider(
                    "pal-bg",
                    (p.bg - 0.03) / 0.13,
                    ui::blend_track(dark.background(), light.background()),
                    p.background(),
                    setter(|p, t| p.bg = 0.03 + t * 0.13),
                )
                .into_any_element(),
            ))
            .child(line(
                "Tint strength",
                format!("{}%", (p.tint / 0.6 * 100.).round()),
                ui::slider(
                    "pal-tint",
                    p.tint / 0.6,
                    ui::blend_track(gray.surface(0.2), full.surface(0.2)),
                    p.surface(0.2),
                    setter(|p, t| p.tint = t * 0.6),
                )
                .into_any_element(),
            ))
            .child(line(
                "Accent",
                format!("#{:06x}", p.accent),
                div()
                    .flex()
                    .gap(crate::ui::sp(8.))
                    .children(ACCENTS.into_iter().enumerate().map(|(i, c)| {
                        ui::swatch(("pal-accent", i), gpui::rgb(c), p.accent == c).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.edit_palette(|p| p.accent = c, cx)
                            }),
                        )
                    }))
                    .into_any_element(),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(readable.1)
                    .child(
                        icon(
                            if text_ok && accent_ok {
                                "checkmark-circle01"
                            } else {
                                "alert-circle"
                            },
                            13.,
                        )
                        .text_color(readable.1),
                    )
                    .child(readable.0),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(
                        ui::button("pal-copy", "Copy share line", ButtonKind::Outline, true)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    copied.clone(),
                                ));
                                this.palette_note = Some((
                                    "Copied. Anyone can paste it here to get this palette.".into(),
                                    false,
                                ));
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        ui::button(
                            "pal-delete",
                            if armed { "Delete it?" } else { "Delete" },
                            ButtonKind::Danger,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.confirm_delete {
                                this.palettes.remove(&id);
                                this.confirm_delete = false;
                                this.apply_palette(cx);
                            } else {
                                this.confirm_delete = true;
                                cx.notify();
                            }
                        })),
                    ),
            )
            .into_any_element()
    }

    /// Changes how round, roomy or spread out the app is, everywhere at once.
    fn set_shape(
        &mut self,
        change: impl FnOnce(&mut crate::palettes::Saved),
        cx: &mut Context<Self>,
    ) {
        change(&mut self.palettes);
        self.save_palette_later(cx);
    }

    /// The Corners, Spacing and Window gap rows.
    fn render_shape(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = &self.palettes;
        let this = cx.entity().downgrade();
        let setter = move |f: fn(&mut crate::palettes::Saved, f32)| {
            let this = this.clone();
            move |t: f32, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.set_shape(|s| f(s, t), cx));
            }
        };
        let presets = |id: &'static str,
                       items: [(&'static str, f32); 3],
                       now: f32,
                       set: fn(&mut crate::palettes::Saved, f32),
                       cx: &mut Context<Self>| {
            let mut seg = ui::segmented();
            for (i, (label, v)) in items.into_iter().enumerate() {
                seg = seg.child(
                    ui::segment((id, i), None, label, (now - v).abs() < 0.01).on_click(
                        cx.listener(move |this, _, _, cx| this.set_shape(|s| set(s, v), cx)),
                    ),
                );
            }
            seg
        };
        let line = |title: &'static str,
                    hint: &'static str,
                    value: String,
                    seg: gpui::Div,
                    slider: gpui::Stateful<gpui::Div>| {
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(8.))
                .px(crate::ui::sp(14.))
                .py(crate::ui::sp(12.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(12.))
                        .child(label_block(title, hint))
                        .child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_subtle())
                                .child(value),
                        )
                        .child(seg),
                )
                .child(slider)
                .into_any_element()
        };
        let track = || ui::blend_track(theme::fill_raised(), theme::fill_selected());
        vec![
            line(
                "Corners",
                "How round cards, buttons and fields are.",
                format!("{:.0}%", s.radius * 100.),
                presets(
                    "shape-radius",
                    [("Sharp", 0.5), ("Default", 1.), ("Round", 1.5)],
                    s.radius,
                    |s, v| s.radius = v,
                    cx,
                ),
                ui::pinned_slider(
                    "shape-radius-slider",
                    s.radius / 2.,
                    track(),
                    theme::active(),
                    &self.pane_scroll,
                    setter(|s, t| s.radius = (t * 2. * 20.).round() / 20.),
                ),
            ),
            line(
                "Spacing",
                "The room inside and between things.",
                format!("{:.0}%", s.space * 100.),
                presets(
                    "shape-space",
                    [("Compact", 0.8), ("Default", 1.), ("Roomy", 1.25)],
                    s.space,
                    |s, v| s.space = v,
                    cx,
                ),
                ui::pinned_slider(
                    "shape-space-slider",
                    s.space - 0.6,
                    track(),
                    theme::active(),
                    &self.pane_scroll,
                    setter(|s, t| s.space = ((0.6 + t) * 20.).round() / 20.),
                ),
            ),
            line(
                "Window gap",
                "The space around and between the sidebar, the main area and the panel.",
                format!("{:.0} px", s.window_gap),
                presets(
                    "shape-gap",
                    [("None", 0.), ("Default", 8.), ("Wide", 14.)],
                    s.window_gap,
                    |s, v| s.window_gap = v,
                    cx,
                ),
                ui::pinned_slider(
                    "shape-gap-slider",
                    s.window_gap / 20.,
                    track(),
                    theme::active(),
                    &self.pane_scroll,
                    setter(|s, t| s.window_gap = (t * 20.).round()),
                ),
            ),
        ]
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = theme::diff_palette();
        let glass = theme::glass();
        let segments = ui::segmented().children(DiffPalette::ALL.into_iter().map(|p| {
            let active = p == palette;
            let (add, del) = p.swatch();
            div()
                .id(p.id())
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(4.))
                .h(px(ui::CONTROL_H - 4.))
                .px(crate::ui::sp(8.))
                .rounded(crate::ui::rad(ui::RADIUS_SM))
                .text_size(px(ui::TEXT_SM))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .when(active, |d| {
                    d.bg(theme::fill_selected()).text_color(theme::content())
                })
                .when(!active, |d| {
                    d.text_color(theme::text_subtle())
                        .hover(|st| st.text_color(theme::content()))
                })
                .child(div().size(px(8.)).rounded_full().bg(add))
                .child(div().size(px(8.)).rounded_full().bg(del))
                .child(div().ml(px(2.)).child(p.label()))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(SettingsEvent::DiffPalette(p));
                    cx.notify();
                }))
        }));
        let preview_line = |mark: &'static str, text: &'static str, add: bool| {
            let (fg, bg, bar) = if add {
                (
                    theme::diff_add(),
                    theme::diff_add_bg(),
                    theme::diff_add_bar(),
                )
            } else {
                (
                    theme::diff_del(),
                    theme::diff_del_bg(),
                    theme::diff_del_bar(),
                )
            };
            div()
                .flex()
                .items_center()
                .h(px(20.))
                .bg(bg)
                .border_l_2()
                .border_color(bar)
                .child(
                    div()
                        .w(px(20.))
                        .flex()
                        .justify_center()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(fg)
                        .child(mark),
                )
                .child(div().text_color(theme::text_muted()).child(text))
        };
        let preview = div()
            .flex()
            .flex_col()
            .py(crate::ui::sp(4.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .overflow_hidden()
            .border_1()
            .border_color(theme::stroke())
            .bg(theme::background())
            .font_family(theme::FONT_MONO)
            .text_size(px(ui::TEXT_XS))
            .child(
                div()
                    .h(px(20.))
                    .border_l_2()
                    .border_color(gpui::transparent_black())
                    .pl(crate::ui::sp(20.))
                    .flex()
                    .items_center()
                    .text_color(theme::text_subtle())
                    .child("fn retry(n: u32) {"),
            )
            .child(preview_line("−", "    let wait = 100;", false))
            .child(preview_line("+", "    let wait = 100 * 2u64.pow(n);", true))
            .child(
                div()
                    .h(px(20.))
                    .border_l_2()
                    .border_color(gpui::transparent_black())
                    .pl(crate::ui::sp(20.))
                    .flex()
                    .items_center()
                    .text_color(theme::text_subtle())
                    .child("}"),
            );
        let mode = {
            let mut seg = ui::segmented();
            for (i, m) in crate::theme::Mode::ALL.into_iter().enumerate() {
                seg = seg.child(
                    ui::segment(("set-mode", i), None, m.label(), crate::theme::mode() == m)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_shape(|s| s.light = m == crate::theme::Mode::Light, cx);
                        })),
                );
            }
            row(
                "Mode",
                "Dark draws the palette on black; Light on white with dark text.",
                seg.into_any_element(),
            )
            .into_any_element()
        };
        let grays = {
            let mut seg = ui::segmented();
            for (i, g) in crate::theme::Grays::ALL.into_iter().enumerate() {
                seg = seg.child(
                    ui::segment(
                        ("set-grays", i),
                        None,
                        g.label(),
                        crate::theme::grays() == g,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SettingsEvent::Grays(g));
                        cx.notify();
                    })),
                );
            }
            row(
                "Grays",
                "How bright secondary text and borders are. Crisp lifts labels on the dark background; Soft quiets them.",
                seg.into_any_element(),
            )
            .into_any_element()
        };
        let text_size = {
            let mut seg = ui::segmented();
            for (i, t) in crate::ui::TypeScale::ALL.into_iter().enumerate() {
                seg = seg.child(
                    ui::segment(
                        ("set-type", i),
                        None,
                        t.label(),
                        crate::ui::type_scale() == t,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SettingsEvent::TypeScale(t));
                        cx.notify();
                    })),
                );
            }
            row(
                "Text size",
                "Comfortable sets body text at 14 instead of 13.",
                seg.into_any_element(),
            )
            .into_any_element()
        };
        let diffs = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .px(crate::ui::sp(14.))
            .py(crate::ui::sp(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(16.))
                    .child(label_block(
                        "Diff colors",
                        "Colorblind uses blue and orange. Lines keep their + and − marks.",
                    ))
                    .child(segments),
            )
            .child(preview)
            .into_any_element();
        let glass_row = row(
            "Glass",
            "See-through window and sidebar; the compositor blurs what is behind (KDE, Hyprland).",
            switch(glass).into_any_element(),
        )
        .id("set-glass")
        .cursor_pointer()
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.emit(SettingsEvent::Glass(!glass));
            cx.notify();
        }))
        .into_any_element();
        let gallery = row(
            "Design system",
            "Every color, type style and component Arakun is built from, with the gray and type choices.",
            ui::button("set-gallery", "Open", ButtonKind::Outline, true)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::OpenGallery)))
                .into_any_element(),
        )
        .into_any_element();
        stack(vec![
            titled("Colors", vec![self.render_palettes(cx), mode]),
            titled("Text", vec![text_size, grays]),
            titled("Shape", self.render_shape(cx)),
            titled("Code", vec![diffs]),
            titled("Window", vec![glass_row, gallery]),
        ])
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
            .when(busy, |d| {
                d.child(ui::spinner("set-voice-busy", 12., theme::mix(0.5)))
            })
            .child(
                ui::button(
                    "set-voice-test-es",
                    "Español",
                    ButtonKind::Outline,
                    can_test,
                )
                .when(can_test, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.test_voice(Lang::Es, cx)))
                }),
            )
            .child(
                ui::button(
                    "set-voice-test-en",
                    "English",
                    ButtonKind::Outline,
                    can_test,
                )
                .when(can_test, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.test_voice(Lang::En, cx)))
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
                    row(
                        "Read questions aloud",
                        "When Droid asks you something, Arakun says the question and its options, even with the session on screen. It stops once you answer.",
                        switch(enabled).into_any_element(),
                    )
                    .id("set-voice")
                    .cursor_pointer()
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
        let mut groups: Vec<(&str, Vec<&Shortcut>)> = Vec::new();
        for sc in SHORTCUTS.iter().filter(|sc| Self::shortcut_matches(&q, sc)) {
            match groups.iter_mut().find(|(g, _)| *g == sc.0) {
                Some((_, v)) => v.push(sc),
                None => groups.push((sc.0, vec![sc])),
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
                            .map(|(_, keys, what)| {
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
                                            .child(*what),
                                    )
                                    .child(keycaps(keys))
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
            format!("Arakun {}", env!("CARGO_PKG_VERSION")).into()
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

        ui::modal_panel("settings", DIALOG_W)
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

/// The tint at slider position `t`. 360° is 0° again, which would send the
/// thumb back to the left end, so the right end stops just short of it.
fn hue_at(t: f32) -> f32 {
    (t * 360.).clamp(0., 359.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tint_slider_stops_short_of_360() {
        assert_eq!(hue_at(0.), 0.);
        assert_eq!(hue_at(0.5), 180.);
        assert_eq!(hue_at(1.), 359.);
    }

    #[test]
    fn shortcut_search_reads_keys_and_words() {
        let sc = &("Tabs and panes", "Ctrl+W", "Close tab");
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
        assert!(found("redacted").contains(&Section::Privacy));
        assert!(found("stream").contains(&Section::Privacy));
        assert!(!found("crash").contains(&Section::General));
        assert!(found("factory").contains(&Section::Droid));
        assert!(found("ctrl+w").contains(&Section::Shortcuts));
        assert_eq!(found("").len(), Section::ALL.len());
        assert!(found("zzz-nothing").is_empty());
    }
}
