//! The session composer: an entity of its own that keeps each environment's
//! draft and what waits with it (images, PDFs, pastes, dropped sessions, notes),
//! with its keys, mouse, history and completion. The shell tells it each
//! frame what is around it (`Frame`), and it asks the shell, by its events,
//! for what reaches past it: sending, menus, pickers and saving.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::EventEmitter;

use super::*;
use crate::agent::notes::ChatNote;

mod completion;
mod input;
mod paste;
mod secret;
mod view;

pub(super) use paste::save_image;

use completion::Completion;
pub(super) use completion::host_skills_reach;

/// What an empty composer says; the setup's preview of the window shows it too.
pub(super) const PLACEHOLDER: &str = "Ask, build, / for commands, @ for references...";
const DRAFT_SAVE_DELAY: Duration = Duration::from_millis(600);

/// What one environment's composer holds until it is sent.
#[derive(Default)]
pub(super) struct EnvDraft {
    pub text: crate::draft::Draft,
    /// Images and PDFs waiting above the text, sent along with the draft.
    pub files: Vec<PathBuf>,
    /// Large pastes, sent with the draft.
    pub pastes: Vec<paste::Paste>,
    /// Other sessions' conversations dropped on the composer.
    pub attached: Vec<drag::AttachedContext>,
    /// Annotations and review comments, sent with the next prompt.
    pub notes: Vec<ChatNote>,
    /// The draft holds a secret, and the notice about it is on show.
    pub secret_notice: Option<secret::SecretNotice>,
}

impl EnvDraft {
    /// The prompt this sends from `env`: the text, then the files as
    /// mentions, the dropped sessions, the pastes and the notes, a blank
    /// line between each.
    fn outgoing(&self, env: &Path) -> String {
        let mut parts = vec![self.text.trim().to_string()];
        if !self.files.is_empty() {
            // Mentioned like any file: the agent gets the files and their paths.
            let mentions: Vec<String> = self
                .files
                .iter()
                .map(|p| {
                    crate::agent::attach::mention(
                        &p.strip_prefix(env).unwrap_or(p).to_string_lossy(),
                    )
                })
                .collect();
            parts.push(mentions.join(" "));
        }
        parts.extend(self.attached.iter().map(|c| c.block.clone()));
        parts.extend(
            self.pastes
                .iter()
                .map(|p| crate::agent::attach::wrap_paste(&p.text)),
        );
        if !self.notes.is_empty() {
            parts.push(crate::agent::notes::format(&self.notes));
        }
        parts.retain(|p| !p.is_empty());
        parts.join("\n\n")
    }

    /// Something waits to go with the text.
    fn has_extras(&self) -> bool {
        !self.files.is_empty()
            || !self.pastes.is_empty()
            || !self.attached.is_empty()
            || !self.notes.is_empty()
    }
}

/// What the shell around the composer says about this frame: where it
/// writes, the agent and its chips, and where `/` completion looks.
pub(super) struct Frame {
    /// The environment on screen, whose draft the composer edits.
    pub env: PathBuf,
    pub has_projects: bool,
    /// On the start screen, with more room around the text.
    pub home: bool,
    pub agent: AgentKind,
    /// The composer writes to a session, not a new one.
    pub in_session: bool,
    /// The id of that session.
    pub session: Option<SharedString>,
    /// The environment is on a server, where this computer's secrets are not.
    pub remote: bool,
    pub busy: bool,
    pub model_label: Option<String>,
    pub effort_label: Option<String>,
    pub fast: bool,
    pub permissions: Option<(String, bool)>,
    pub usage_line: Option<String>,
    /// The Claude seat the session runs on, while it runs (`agent::seats`).
    pub seat: Option<String>,
    /// Why the agent cannot run now, the other agent, and whether it can.
    pub limits: Option<(String, AgentKind, bool)>,
    /// The active session, whose reported commands `/` completes.
    pub thread: Option<Entity<crate::agent::Thread>>,
    /// Where `/` completion reads skills: `Some(true)` what this computer
    /// mirrors to servers, `Some(false)` its own, `None` nowhere.
    pub skills: Option<bool>,
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            env: PathBuf::new(),
            has_projects: false,
            home: false,
            agent: AgentKind::Claude,
            in_session: false,
            session: None,
            remote: false,
            busy: false,
            model_label: None,
            effort_label: None,
            fast: false,
            permissions: None,
            usage_line: None,
            seat: None,
            limits: None,
            thread: None,
            skills: None,
        }
    }
}

/// What the composer asks of the shell.
pub(super) enum ComposerEvent {
    /// The caret moved along its line, as a fraction, for the raccoon.
    Typed(f32),
    /// Typing paused: the draft can be saved.
    Save,
    /// Send the draft; with `now`, stop a running turn for it.
    Submit {
        now: bool,
    },
    /// The send button, while a turn runs and nothing is written.
    Stop,
    /// Escape with nothing selected.
    Escape,
    CyclePermissions,
    ModelMenu(gpui::Point<Pixels>),
    PermissionMenu(gpui::Point<Pixels>),
    AttachFiles,
    DropSession(drag::DraggedSession),
    UseAgent(AgentKind),
    OpenAttachment(PathBuf),
    /// The notes of this environment changed; its transcripts mark them.
    NotesChanged(PathBuf),
    /// `@` completion wants a server environment's files.
    ListFiles(PathBuf),
}

/// A draft taken out of the composer to be sent.
pub(super) struct Outgoing {
    pub text: String,
    /// Notes went with it, so their marks go.
    pub had_notes: bool,
}

/// The composer's own state: the drafts, focus, caret, scroll, history and
/// open completion.
pub(super) struct Composer {
    pub focus: FocusHandle,
    /// What the shell said last (`Frame`).
    frame: Frame,
    /// Each environment's draft, by its path.
    drafts: HashMap<PathBuf, EnvDraft>,
    /// The prompts saved for `!` (`prompts.json`).
    prompts: Vec<crate::prompts::SavedPrompt>,
    /// The composer's text as last painted, for clicks, Up/Down and the IME.
    layout: Rc<RefCell<Option<TextLayout>>>,
    scroll: ScrollHandle,
    /// Scroll the composer to its caret on the next paint; set by edits and
    /// caret moves, not by the wheel.
    reveal: Rc<Cell<bool>>,
    /// A mouse selection is being dragged in the composer.
    selecting: bool,
    /// Where Up/Down aim, so the caret keeps its column across shorter lines.
    goal_x: Option<Pixels>,
    history: crate::history::History,
    /// The history entry Up/Down brought into the composer.
    history_at: Option<usize>,
    /// Saves the state shortly after the draft stops changing.
    draft_save: Option<Task<()>>,
    /// The composer's caret, shared with the canvas that paints it.
    caret: Rc<RefCell<crate::caret::Caret>>,
    /// Open `@`/`/` completion of the composer.
    completion: Option<Completion>,
    /// `git ls-files` of an environment, cached briefly for `@` completion.
    file_index: Option<(PathBuf, Instant, Arc<Vec<String>>)>,
    /// The environment whose file index is being read, if any.
    file_index_loading: Option<PathBuf>,
    /// The skills an environment's agents read, cached briefly for `/` completion.
    skill_index: Option<(PathBuf, Instant, Arc<Vec<crate::agent::Choice>>)>,
    skill_index_loading: bool,
    /// A one-line note under the composer (a prompt was saved, ...), until the next key.
    flash: Option<SharedString>,
    /// The secrets each session was told to send anyway.
    allowed: secret::Allowed,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Focusable for Composer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Composer {
    /// `drafts` are the ones saved with the state.
    pub fn new(drafts: state::Drafts, cx: &mut Context<Self>) -> Self {
        let mut drafts: HashMap<PathBuf, EnvDraft> = drafts
            .into_iter()
            .map(|(path, text, images)| {
                let d = EnvDraft {
                    text,
                    files: images,
                    ..Default::default()
                };
                (path, d)
            })
            .collect();
        drafts.entry(PathBuf::new()).or_default();
        Self {
            focus: cx.focus_handle(),
            frame: Frame::default(),
            drafts,
            prompts: crate::prompts::load(),
            layout: Rc::default(),
            scroll: ScrollHandle::new(),
            reveal: Rc::default(),
            selecting: false,
            goal_x: None,
            history: crate::history::History::load(),
            history_at: None,
            draft_save: None,
            caret: Default::default(),
            completion: None,
            file_index: None,
            file_index_loading: None,
            skill_index: None,
            skill_index_loading: false,
            flash: None,
            allowed: secret::Allowed::default(),
        }
    }

    // ---- what the shell reads and writes ---------------------------------

    /// What the shell says about this frame.
    pub fn set_frame(&mut self, frame: Frame) {
        if frame.env != self.frame.env {
            self.drafts.entry(frame.env.clone()).or_default();
        }
        self.allowed.adopt(&frame);
        self.frame = frame;
    }

    /// Another environment is on screen: its draft, and no completion of the last one.
    pub fn show_env(&mut self, env: &Path) {
        self.drafts.entry(env.to_path_buf()).or_default();
        self.frame.env = env.to_path_buf();
        self.completion = None;
    }

    pub fn draft(&self, env: &Path) -> Option<&EnvDraft> {
        self.drafts.get(env)
    }

    pub fn draft_mut(&mut self, env: &Path) -> &mut EnvDraft {
        self.drafts.entry(env.to_path_buf()).or_default()
    }

    /// The draft of the environment on screen.
    fn here(&self) -> &EnvDraft {
        // `set_frame`, `show_env` and `new` keep an entry for it.
        &self.drafts[&self.frame.env]
    }

    fn here_mut(&mut self) -> &mut EnvDraft {
        self.drafts.entry(self.frame.env.clone()).or_default()
    }

    /// Drops the drafts of environments that are gone, as they went with
    /// their environment when it held them. The one on screen stays.
    pub fn retain_drafts(&mut self, keep: impl Fn(&Path) -> bool) {
        let here = self.frame.env.clone();
        self.drafts.retain(|env, _| *env == here || keep(env));
    }

    /// An environment's draft as the state saves it: text, caret and attached files.
    pub fn saved(&self, env: &Path) -> Option<(String, usize, Vec<PathBuf>)> {
        let d = self.drafts.get(env)?;
        Some((d.text.to_string(), d.text.head(), d.files.clone()))
    }

    /// Where the caret is on screen, for the start screen's face to look at.
    pub fn caret_position(&self) -> Option<gpui::Point<Pixels>> {
        self.layout
            .borrow()
            .as_ref()
            .and_then(|l| l.position_for_index(self.here().text.head()))
    }

    /// A one-line note under the composer, until the next key.
    pub fn flash(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.flash = Some(text.into());
        cx.notify();
    }

    pub fn close_completion(&mut self) {
        self.completion = None;
    }

    /// The draft as it goes out, with what waits with it, and the composer
    /// emptied for the next one. `None` when there is nothing to send.
    pub fn take_prompt(&mut self, env: &Path, cx: &mut Context<Self>) -> Option<Outgoing> {
        let d = self.draft_mut(env);
        if d.text.trim().is_empty() && !d.has_extras() {
            return None;
        }
        let typed = d.text.trim().to_string();
        let text = d.outgoing(env);
        let had_notes = !d.notes.is_empty();
        d.files.clear();
        d.attached.clear();
        d.pastes.clear();
        d.notes.clear();
        d.text.clear();
        d.secret_notice = None;
        self.completion = None;
        self.changed(cx);
        self.history_at = None;
        if self.history.push(&typed) {
            let bytes = self.history.bytes();
            cx.background_spawn(crate::guard::task(
                async move { crate::history::write(&bytes) },
            ))
            .detach();
        }
        Some(Outgoing { text, had_notes })
    }

    /// After an edit or a caret move: show the caret, and save the draft once
    /// typing pauses.
    pub fn changed(&mut self, cx: &mut Context<Self>) {
        // The raccoon watches you type: how far along its line the caret is.
        let draft = &self.here().text;
        if !draft.is_empty() {
            let head = draft.head().min(draft.len());
            let line = draft[..head].rfind('\n').map_or(0, |i| i + 1);
            let at = (draft[line..head].chars().count() % 40) as f32 / 40.;
            cx.emit(ComposerEvent::Typed(at));
        }
        self.caret.borrow_mut().hold();
        self.reveal.set(true);
        self.draft_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_SAVE_DELAY).await;
            let _ = this.update(cx, |_, cx| cx.emit(ComposerEvent::Save));
        }));
    }

    // ---- keys -------------------------------------------------------------

    fn key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        // The completion popup owns these keys while it is open.
        enum CompKey {
            Up,
            Down,
            Accept,
            Close,
        }
        let prompt_key = match k.key.as_str() {
            "p" => Some(true),
            "d" => Some(false),
            _ => None,
        }
        .filter(|_| k.modifiers.alt && !k.modifiers.control)
        .and_then(|pin| Some((self.completion.as_ref()?.selected_prompt()?, pin)));
        if let Some((i, pin)) = prompt_key {
            cx.stop_propagation();
            self.pin_or_delete_prompt(i, pin, cx);
            return;
        }
        let comp_key = if self.completion.is_some() {
            match k.key.as_str() {
                "up" => Some(CompKey::Up),
                "down" => Some(CompKey::Down),
                "tab" => Some(CompKey::Accept),
                // Ctrl+Enter sends now, past the popup.
                "enter" if !k.modifiers.shift && !crate::platform::command_key(&k.modifiers) => {
                    Some(CompKey::Accept)
                }
                "escape" => Some(CompKey::Close),
                _ => None,
            }
        } else {
            None
        };
        if let Some(action) = comp_key {
            cx.stop_propagation();
            match action {
                CompKey::Up => {
                    if let Some(c) = &mut self.completion {
                        c.step(false);
                    }
                }
                CompKey::Down => {
                    if let Some(c) = &mut self.completion {
                        c.step(true);
                    }
                }
                CompKey::Accept => self.accept_completion(cx),
                CompKey::Close => self.completion = None,
            }
            cx.notify();
            return;
        }
        let m = &k.modifiers;
        // Ctrl, or Cmd on macOS, with nothing but Shift.
        let ctrl_only = crate::platform::command_key(m);
        // Shift+Tab steps through the permission levels, as in Claude Code and Droid.
        if k.key == "tab" && m.shift && !m.control && !m.alt && !m.platform {
            cx.stop_propagation();
            cx.emit(ComposerEvent::CyclePermissions);
            cx.notify();
            return;
        }
        if self.flash.take().is_some() {
            cx.notify();
        }
        if ctrl_only && !m.shift && k.key == "enter" {
            cx.stop_propagation();
            self.completion = None;
            cx.emit(ComposerEvent::Submit { now: true });
            return;
        }
        if ctrl_only && k.key == "s" {
            cx.stop_propagation();
            self.save_draft_as_prompt(cx);
            return;
        }
        let history = match k.key.as_str() {
            "z" | "Z" if ctrl_only => Some(m.shift),
            "y" if ctrl_only && !m.shift => Some(true),
            _ => None,
        };
        if let Some(redo) = history {
            cx.stop_propagation();
            let draft = &mut self.here_mut().text;
            if if redo { draft.redo() } else { draft.undo() } {
                self.changed(cx);
                self.update_completion(cx);
                cx.notify();
            }
            return;
        }
        let clipboard = match k.key.as_str() {
            "v" if ctrl_only => Some(0),
            "insert" if m.shift && !m.control => Some(0),
            "c" if ctrl_only => Some(1),
            "insert" if m.control && !m.shift => Some(1),
            "x" if ctrl_only && !m.shift => Some(2),
            "a" if ctrl_only && !m.shift => Some(3),
            _ => None,
        };
        if let Some(op) = clipboard {
            cx.stop_propagation();
            match op {
                0 => self.paste(cx.read_from_clipboard(), cx),
                1 => self.copy(false, cx),
                2 => self.copy(true, cx),
                _ => {
                    self.here_mut().text.select_all();
                    self.to_primary(cx);
                    self.update_completion(cx);
                    cx.notify();
                }
            }
            return;
        }
        // Alt+↑/↓ and the like belong to the window. On macOS Option+←/→ and
        // Option+Backspace go by word, and Cmd+arrows to the ends.
        let mac = cfg!(target_os = "macos") && !(m.alt && m.platform);
        let mac_edit = mac
            && match k.key.as_str() {
                "left" | "right" | "backspace" | "delete" => true,
                "up" | "down" => m.platform,
                _ => false,
            };
        if (m.alt || m.platform) && !mac_edit {
            return;
        }
        let (key, word) = match (mac && m.platform, k.key.as_str()) {
            (true, "left") => ("home", false),
            (true, "right") => ("end", false),
            (true, "up") => ("home", true),
            (true, "down") => ("end", true),
            (true, "backspace") => {
                self.here_mut().text.home(true);
                ("backspace", false)
            }
            (_, key) => (key, crate::platform::word_key(m)),
        };
        if !matches!(key, "up" | "down") {
            self.goal_x = None;
        }
        let extend = m.shift;
        if matches!(key, "up" | "down") && !word && !extend && self.recall(key == "up") {
            cx.stop_propagation();
            self.changed(cx);
            self.completion = None;
            cx.notify();
            return;
        }
        if matches!(key, "up" | "down") && !word {
            self.vertical(key == "up", extend);
        } else {
            let notice = self.secret_notice_open();
            let draft = &mut self.here_mut().text;
            match key {
                "left" => draft.left(word, extend),
                "right" => draft.right(word, extend),
                "home" if word => draft.move_to(0, extend),
                "home" => draft.home(extend),
                "end" if word => draft.move_to(usize::MAX, extend),
                "end" => draft.end(extend),
                "backspace" => draft.backspace(word),
                "delete" => draft.delete(word),
                "enter" if extend => draft.insert("\n"),
                "enter" => cx.emit(ComposerEvent::Submit { now: false }),
                "escape" if draft.has_selection() => {
                    let head = draft.head();
                    draft.move_to(head, false);
                }
                // Escape is the notice's Cancel.
                "escape" if notice => self.here_mut().secret_notice = None,
                "escape" => cx.emit(ComposerEvent::Escape),
                _ => return,
            }
        }
        cx.stop_propagation();
        self.changed(cx);
        self.update_completion(cx);
        cx.notify();
    }

    /// Up in an empty composer brings back the last prompt sent, and more Ups
    /// older ones; Down walks back toward an empty composer. Only while the
    /// recalled text is untouched and the caret is on its first line (Up) or
    /// last line (Down).
    fn recall(&mut self, up: bool) -> bool {
        let draft = &self.here().text;
        let showing = self
            .history_at
            .filter(|&i| self.history.get(i) == Some(&**draft));
        let (before, after) = draft.split_at(draft.head());
        let edge = if up {
            !before.contains('\n')
        } else {
            !after.contains('\n')
        };
        let next = match showing {
            None if up && draft.is_empty() => match self.history.older(None) {
                Some(i) => Some(i),
                None => return false,
            },
            Some(i) if edge && up => Some(self.history.older(Some(i)).unwrap_or(i)),
            Some(i) if edge => self.history.newer(i),
            _ => return false,
        };
        self.history_at = next;
        let text = next
            .and_then(|i| self.history.get(i))
            .unwrap_or("")
            .to_string();
        self.here_mut().text.set(text);
        true
    }

    /// Up/Down in the composer: the line above or below at the same column,
    /// or the start or end of the draft past its first or last line.
    fn vertical(&mut self, up: bool, extend: bool) {
        let layout = self.layout.borrow().clone();
        let head = self.here().text.head();
        let target = match layout
            .as_ref()
            .and_then(|l| Some((l, l.position_for_index(head)?)))
        {
            Some((layout, at)) => {
                let x = *self.goal_x.get_or_insert(at.x);
                let line = layout.line_height();
                let y = if up {
                    at.y - line / 2.
                } else {
                    at.y + line * 1.5
                };
                let bounds = layout.bounds();
                if y < bounds.top() {
                    0
                } else if y > bounds.bottom() {
                    usize::MAX
                } else {
                    layout
                        .index_for_position(point(x, y))
                        .unwrap_or_else(|ix| ix)
                }
            }
            None if up => 0,
            None => usize::MAX,
        };
        self.here_mut().text.move_to(target, extend);
    }

    // ---- mouse ------------------------------------------------------------

    /// The draft index under `position`, from the text as last painted.
    fn index_at(&self, position: gpui::Point<Pixels>) -> Option<usize> {
        let layout = self.layout.borrow().clone()?;
        Some(layout.index_for_position(position).unwrap_or_else(|ix| ix))
    }

    /// A click in the composer's text: places the caret (Shift extends the
    /// selection), a double click takes the word, a triple click the line.
    fn click(
        &mut self,
        position: gpui::Point<Pixels>,
        clicks: usize,
        shift: bool,
        cx: &mut Context<Self>,
    ) {
        self.goal_x = None;
        let Some(ix) = self.index_at(position) else {
            return;
        };
        self.caret.borrow_mut().hold();
        let draft = &mut self.here_mut().text;
        match clicks {
            0 | 1 => draft.move_to(ix, shift),
            2 => draft.select(draft.word_at(ix)),
            _ => draft.select(draft.line_at(ix)),
        }
        self.selecting = true;
        self.update_completion(cx);
        cx.notify();
    }

    fn drag_to(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(ix) = self.index_at(position) {
            self.here_mut().text.move_to(ix, true);
            self.changed(cx);
            cx.notify();
        }
    }

    fn release(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.selecting) {
            self.to_primary(cx);
            self.update_completion(cx);
        }
    }

    /// Linux's primary selection: what is selected can be pasted elsewhere with a middle click.
    fn to_primary(&self, cx: &mut Context<Self>) {
        let draft = &self.here().text;
        if draft.has_selection() {
            crate::platform::write_primary(
                cx,
                ClipboardItem::new_string(draft.selected_text().to_string()),
            );
        }
    }

    // ---- saved prompts ----------------------------------------------------

    /// Alt+P pins the saved prompt the popup has selected, Alt+D deletes it.
    fn pin_or_delete_prompt(&mut self, i: usize, pin: bool, cx: &mut Context<Self>) {
        if pin {
            self.prompts[i].pinned = !self.prompts[i].pinned;
        } else {
            let gone = self.prompts.remove(i);
            self.flash = Some(format!("Deleted !{}", gone.name).into());
        }
        crate::prompts::store(&self.prompts);
        let selected = self.completion.as_ref().map_or(0, |c| c.selected);
        self.update_completion(cx);
        if let Some(c) = &mut self.completion {
            c.select(selected);
        }
        cx.notify();
    }

    /// Keeps the draft as a saved prompt, named after its first words.
    fn save_draft_as_prompt(&mut self, cx: &mut Context<Self>) {
        let text = self.here().text.trim().to_string();
        if text.is_empty() || text.starts_with('!') {
            self.flash = Some(
                crate::platform::shortcut_label("Write a prompt first; Ctrl+S saves it for !")
                    .into(),
            );
        } else if let Some(p) = self.prompts.iter().find(|p| p.text == text) {
            self.flash = Some(format!("Already saved as !{}", p.name).into());
        } else {
            let name = crate::prompts::name_for(&text, &self.prompts);
            self.prompts.push(crate::prompts::SavedPrompt {
                name: name.clone(),
                text,
                pinned: false,
                uses: 0,
                last_used: model::unix_now(),
            });
            crate::prompts::store(&self.prompts);
            self.flash = Some(format!("Saved as !{name}. Type ! to use it.").into());
        }
        cx.notify();
    }

    // ---- what waits with the draft ----------------------------------------

    pub fn remove_note(&mut self, ix: usize, cx: &mut Context<Self>) {
        let notes = &mut self.here_mut().notes;
        if ix < notes.len() {
            notes.remove(ix);
        }
        cx.emit(ComposerEvent::NotesChanged(self.frame.env.clone()));
        cx.notify();
    }

    pub fn clear_notes(&mut self, cx: &mut Context<Self>) {
        self.here_mut().notes.clear();
        cx.emit(ComposerEvent::NotesChanged(self.frame.env.clone()));
        cx.notify();
    }
}

/// The shell's side of the composer: what it is told each frame, what it
/// asks for, and which session it writes to.
impl Shell {
    /// The composer under the session it writes to, or on the start screen (`home`).
    pub(super) fn render_composer(&self, home: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = self.composer_frame(home, cx);
        self.composer.update(cx, |c, _| c.set_frame(frame));
        self.composer.clone()
    }

    fn composer_frame(&self, home: bool, cx: &App) -> Frame {
        use crate::agent::Setting;
        let env = self.env();
        let tab = self.composer_tab();
        let session_id = match self.strip.tabs.get(tab).map(|t| &t.kind) {
            Some(TabKind::Session { id, .. }) => Some(id.clone()),
            _ => None,
        };
        let agent = match self.strip.tabs.get(tab).map(|t| &t.kind) {
            Some(TabKind::Session { agent, .. }) => *agent,
            _ => env.agent,
        };
        let thread = session_id.as_ref().and_then(|id| {
            env.sessions[env.session_index(id)?]
                .live
                .as_ref()
                .map(|l| l.thread.clone())
        });
        let limits = self.agent_out_of_limits(agent, cx).map(|why| {
            let other = if agent == AgentKind::Droid {
                AgentKind::Claude
            } else {
                AgentKind::Droid
            };
            (why, other, self.agent_out_of_limits(other, cx).is_none())
        });
        let active = self.active_thread(cx);
        Frame {
            env: env.path.clone(),
            has_projects: self.has_projects(),
            home,
            agent,
            in_session: session_id.is_some(),
            session: session_id,
            remote: env.remote.is_some() || self.project().is_remote(),
            busy: thread.as_ref().is_some_and(|t| t.read(cx).is_busy()),
            model_label: self.setting_value_label(Setting::Model, cx),
            // "Default" effort is whatever the model defaults to; it says nothing on the chip.
            effort_label: self
                .setting_value_label(Setting::Effort, cx)
                .filter(|l| !l.is_empty() && l != "Default"),
            fast: self
                .setting_choices(Setting::Fast, cx)
                .is_some_and(|(current, _)| current == "on"),
            permissions: self.permissions_label(cx),
            usage_line: thread.as_ref().and_then(|t| t.read(cx).usage().summary()),
            seat: thread
                .as_ref()
                .and_then(|t| t.read(cx).seat().map(crate::agent::seats::label))
                .map(|s| crate::redact::value(&s)),
            limits,
            skills: self.composer_skills(active.as_ref(), cx),
            thread: active,
        }
    }

    /// Where `/` completion reads skills (`Frame::skills`).
    fn composer_skills(
        &self,
        thread: Option<&Entity<crate::agent::Thread>>,
        cx: &App,
    ) -> Option<bool> {
        let claude_like = matches!(
            self.composer_agent(),
            AgentKind::Claude | AgentKind::WorkersAi
        );
        if !claude_like {
            return None;
        }
        if self.env().remote.is_some() || self.project().is_remote() {
            // Until the server says what its session has, what this
            // computer mirrored there. An empty list it sent still counts.
            let none_reported = thread.is_none_or(|t| !t.read(cx).commands_known());
            return none_reported.then_some(true);
        }
        // This computer's skills, where the agent sees them.
        let session = match self.strip.tabs.get(self.composer_tab()).map(|t| &t.kind) {
            Some(TabKind::Session { id, .. }) => Some(id.as_ref()),
            _ => None,
        };
        let sandboxed = thread.map(|t| t.read(cx).sandbox().is_some());
        host_skills_reach(self.project(), self.env(), session, sandboxed).then_some(false)
    }

    pub(super) fn on_composer_event(
        &mut self,
        ev: &ComposerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            ComposerEvent::Typed(at) => {
                if let Some(m) = self.composer_mascot(cx) {
                    m.update(cx, |m, cx| {
                        m.key(*at);
                        cx.notify();
                    });
                }
            }
            ComposerEvent::Save => self.save(cx),
            ComposerEvent::Submit { now } => self.submit_with(*now, window, cx),
            ComposerEvent::Stop => {
                self.cancel_active(cx);
            }
            ComposerEvent::Escape => {
                if !self.cancel_active(cx) {
                    window.focus(&self.focus, cx)
                }
            }
            ComposerEvent::CyclePermissions => {
                self.cycle_permissions(cx);
                cx.notify();
            }
            ComposerEvent::ModelMenu(at) => self.open_composer_menu(MenuTarget::ModelMenu, *at, cx),
            ComposerEvent::PermissionMenu(at) => {
                self.open_composer_menu(MenuTarget::PermissionMenu, *at, cx)
            }
            ComposerEvent::AttachFiles => self.attach_files(window, cx),
            ComposerEvent::DropSession(d) => self.drop_session_on_composer(d, window, cx),
            ComposerEvent::UseAgent(agent) => {
                self.env_mut().agent = *agent;
                model::set_default_agent(*agent);
                self.save(cx);
                cx.notify();
            }
            ComposerEvent::OpenAttachment(path) => {
                let env = self.env().path.clone();
                self.open_file_beside(&env, path, None, window, cx);
            }
            ComposerEvent::NotesChanged(env) => self.sync_note_marks(env, cx),
            ComposerEvent::ListFiles(env) => {
                if !self.server_list_files(env) {
                    self.composer.update(cx, |c, _| c.file_index_failed(env));
                }
            }
        }
    }

    pub(super) fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.composer_focus, cx);
    }

    /// A short note under the composer, until the next key.
    pub(super) fn flash(&self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text = text.into();
        self.composer.update(cx, |c, cx| c.flash(text, cx));
    }

    /// After the shell edited the draft on screen: show the caret, save soon.
    pub(super) fn composer_changed(&self, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| c.changed(cx));
    }

    pub(super) fn update_completion(&self, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| c.update_completion(cx));
    }

    /// Edits the text of `env`'s draft.
    pub(super) fn edit_draft<R>(
        &self,
        env: &Path,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut crate::draft::Draft) -> R,
    ) -> R {
        self.composer
            .update(cx, |c, _| f(&mut c.draft_mut(env).text))
    }

    /// Opens the system's file picker; the picked files go into the draft as
    /// `@` mentions, and images and PDFs among them are sent along with the prompt.
    fn attach_files(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.composer.update(cx, |c, _| c.close_completion());
        let env_path = self.env().path.clone();
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            let picked = match rx.await {
                Ok(Ok(picked)) => picked.unwrap_or_default(),
                // Linux without an XDG desktop portal has no file picker.
                Ok(Err(err)) => {
                    eprintln!("arakun: file picker: {err:#}");
                    let _ = this.update(cx, |this, cx| {
                        this.flash(
                            "This system has no file picker. Drag files in or paste them instead.",
                            cx,
                        );
                    });
                    return;
                }
                Err(_) => return,
            };
            if picked.is_empty() {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                // The picker may stay open while someone switches environments.
                if this.env().path != env_path {
                    return;
                }
                this.composer.update(cx, |c, cx| {
                    c.push_mentions(&picked);
                    c.changed(cx);
                });
            });
        })
        .detach();
    }

    /// Sends the draft; with `now`, a running turn is stopped for it instead of
    /// the draft waiting in the queue.
    fn submit_with(&mut self, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        let env_path = self.env().path.clone();
        // A dropped session still being read would miss this prompt.
        if self.dropping.contains_key(&env_path) {
            self.flash(
                "Still reading the dropped session. Send again in a moment.",
                cx,
            );
            return;
        }
        // A server that cannot be reached would lose the prompt: it stays here.
        if let Some(remote) = self.env().remote.clone()
            && self.server_reach(&remote.server) != Some(crate::servers_view::Reach::Connected)
        {
            if let Some(thread) = self.active_thread(cx) {
                thread.update(cx, |t, cx| {
                    t.post_notice(
                        format!(
                            "{} is not connected; the prompt stays in the composer.",
                            remote.machine
                        ),
                        true,
                        cx,
                    )
                });
            }
            return;
        }
        // A secret in the draft waits for the user's answer to the notice.
        if self
            .composer
            .update(cx, |c, cx| c.hold_for_secrets(now, cx))
        {
            return;
        }
        let Some(out) = self
            .composer
            .update(cx, |c, cx| c.take_prompt(&env_path, cx))
        else {
            return;
        };
        if out.had_notes {
            self.sync_note_marks(&env_path, cx);
        }
        if let Some(m) = self.composer_mascot(cx) {
            m.update(cx, |m, cx| {
                m.sent();
                cx.notify();
            });
        }
        self.send_prompt(out.text, now, window, cx);
    }

    /// The tab the composer writes to: the active tab, or the nearest session showing
    /// beside it while the focused pane (a page, a file) is not one.
    pub(super) fn composer_tab(&self) -> usize {
        let strip = &self.strip;
        let session = |i: usize| {
            matches!(
                strip.tabs.get(i).map(|t| &t.kind),
                Some(TabKind::Session { .. })
            )
        };
        let active = strip.active_tab;
        let fresh = matches!(
            strip.tabs.get(active).map(|t| &t.kind),
            Some(TabKind::NewSession)
        );
        // The composer writes to the environment on screen, so a session of
        // another environment in another pane does not take it.
        let here = |i: usize| {
            self.try_env()
                .is_some_and(|e| strip.tabs.get(i).is_some_and(|t| t.is_in(&e.path)))
        };
        if session(active) || fresh {
            return active;
        }
        self.strip
            .split_others_near()
            .into_iter()
            .find(|o| session(*o) && here(*o))
            .unwrap_or(active)
    }

    /// The chat the composer writes to; its raccoon sits on the composer.
    pub(super) fn composer_chat(&self) -> Option<Entity<crate::agent::ThreadView>> {
        let env = self.try_env()?;
        let Some(TabKind::Session { id, .. }) =
            self.strip.tabs.get(self.composer_tab()).map(|t| &t.kind)
        else {
            return None;
        };
        env.session_index(id)
            .and_then(|i| env.sessions[i].live.as_ref())
            .map(|l| l.view.clone())
    }

    pub(super) fn composer_mascot(&self, cx: &App) -> Option<Entity<crate::mascot::Mascot>> {
        self.composer_chat().map(|chat| chat.read(cx).mascot())
    }

    /// Focus left the composer: the raccoon spies toward where it went (the
    /// terminals below, the history above, the pane beside it), or stays put.
    pub(super) fn spy_from_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.composer_chat() else {
            return;
        };
        let env = self.env();
        let dir = if env
            .terminals
            .iter()
            .any(|t| t.view.focus_handle(cx).contains_focused(window, cx))
        {
            [0., 1.]
        } else if chat.read(cx).focus_handle(cx).contains_focused(window, cx) {
            [0., -1.]
        } else if let Some(split) = self
            .strip
            .split
            .as_ref()
            .filter(|_| window.focused(cx).is_some())
        {
            // Toward the focused pane from the composer's.
            let composer = self.composer_tab();
            let at = self.strip.panes().iter().position(|(i, _)| *i == composer);
            let before = match at {
                Some(at) if at != split.focus => split.focus < at,
                _ => split.focus_first(),
            };
            match (split.dir, before) {
                (crate::model::SplitDir::Right, true) => [-1., 0.],
                (crate::model::SplitDir::Right, false) => [1., 0.],
                (crate::model::SplitDir::Down, true) => [0., -1.],
                (crate::model::SplitDir::Down, false) => [0., 1.],
            }
        } else {
            return;
        };
        chat.read(cx).mascot().update(cx, |m, cx| {
            m.set_away(Some(dir));
            cx.notify();
        });
    }

    /// Back in the composer: the raccoon hops back up.
    fn spy_back(&mut self, cx: &mut Context<Self>) {
        if let Some(m) = self.composer_mascot(cx) {
            m.update(cx, |m, cx| {
                m.set_away(None);
                cx.notify();
            });
        }
    }

    /// Typing goes to the session the composer shows, so it becomes the active tab.
    pub(super) fn claim_composer_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.spy_back(cx);
        if !self.has_projects() {
            return;
        }
        let tab = self.composer_tab();
        if tab != self.strip.active_tab {
            self.select_tab(tab, window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_the_text_then_what_waits_with_it() {
        let env = Path::new("/tmp/arakun-composer-test");
        let mut d = EnvDraft::default();
        d.text.set("  fix the tests \n");
        d.files = vec![env.join("shots/a b.png"), PathBuf::from("/elsewhere/c.png")];
        d.attached.push(drag::AttachedContext {
            title: "Other".into(),
            block: "<context/>".into(),
        });
        d.pastes.push(paste::Paste::new("one\ntwo\n".into()));
        assert_eq!(
            d.outgoing(env),
            "fix the tests\n\n@\"shots/a b.png\" @/elsewhere/c.png\n\n<context/>\n\n\
             <pasted-text lines=\"2\">\none\ntwo\n</pasted-text>"
        );

        // Nothing typed: what waits goes alone, without a blank line first.
        d.text.clear();
        d.files.clear();
        d.attached.clear();
        assert_eq!(
            d.outgoing(env),
            "<pasted-text lines=\"2\">\none\ntwo\n</pasted-text>"
        );
    }
}
