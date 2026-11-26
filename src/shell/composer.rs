//! The session composer: keys, mouse, input methods, completion, history,
//! and how the caret is drawn.

use super::*;

/// The composer caret, in pixels.
const CARET_WIDTH: f32 = 1.5;
const CARET_HEIGHT: f32 = 16.;
/// What an empty composer says; the setup's preview of the window shows it too.
pub(super) const PLACEHOLDER: &str = "Ask, build, / for commands, @ for references...";
/// The line the placeholder sits on, which centers the caret before the text.
const PLACEHOLDER_LINE: f32 = 22.;
/// Space above and below the composer's text, kept around the caret when it scrolls.
const COMPOSER_PAD: f32 = 12.;
/// Rows the composer completion popup shows at most.
const COMPLETION_ROWS: usize = 8;
/// `git ls-files` is re-read at most this often while completing.
const FILE_INDEX_TTL: Duration = Duration::from_secs(30);
const DRAFT_SAVE_DELAY: Duration = Duration::from_millis(600);

/// The composer completion popup: files after `@`, agent commands after `/`.
pub(super) struct Completion {
    /// Byte range in the draft of the token being completed (from the `@`/`/`
    /// to the caret); accepting an item replaces it.
    token_start: usize,
    token_end: usize,
    items: Vec<CompletionItem>,
    selected: usize,
    /// Saved prompts (`!`), which can be pinned and deleted from the list.
    prompts: bool,
}

pub(super) struct CompletionItem {
    label: SharedString,
    detail: Option<SharedString>,
    /// Replaces the token, trailing space included.
    insert: String,
    /// The saved prompt this inserts.
    prompt: Option<usize>,
}

/// The composer's own state: its focus, caret, scroll, history and open
/// completion. The draft itself stays on each environment.
pub(super) struct Composer {
    pub focus: FocusHandle,
    /// The composer's text as last painted, for clicks, Up/Down and the IME.
    pub layout: Rc<RefCell<Option<TextLayout>>>,
    pub scroll: ScrollHandle,
    /// Scroll the composer to its caret on the next paint; set by edits and
    /// caret moves, not by the wheel.
    pub reveal: Rc<Cell<bool>>,
    /// A mouse selection is being dragged in the composer.
    pub selecting: bool,
    /// Where Up/Down aim, so the caret keeps its column across shorter lines.
    pub goal_x: Option<Pixels>,
    pub history: crate::history::History,
    /// The history entry Up/Down brought into the composer.
    pub history_at: Option<usize>,
    /// Saves the state shortly after the draft stops changing.
    pub draft_save: Option<Task<()>>,
    /// The composer's caret, shared with the canvas that paints it.
    pub caret: Rc<RefCell<crate::caret::Caret>>,
    /// Open `@`/`/` completion of the composer.
    pub completion: Option<Completion>,
    /// `git ls-files` of an environment, cached briefly for `@` completion.
    pub file_index: Option<(std::path::PathBuf, Instant, Arc<Vec<String>>)>,
    /// The environment whose file index is being read, if any.
    pub file_index_loading: Option<std::path::PathBuf>,
    /// The skills an environment's agents read, cached briefly for `/` completion.
    pub skill_index: Option<(std::path::PathBuf, Instant, Arc<Vec<crate::agent::Choice>>)>,
    pub skill_index_loading: bool,
    /// A one-line note under the composer (a prompt was saved, ...), until the next key.
    pub flash: Option<SharedString>,
    /// Focusing the composer claims its tab; leaving it lets the tab go.
    _focus: [Subscription; 2],
}

impl Composer {
    pub fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        let focus = cx.focus_handle();
        let subs = [
            cx.on_focus(&focus, window, |this, window, cx| {
                this.claim_composer_tab(window, cx)
            }),
            cx.on_blur(&focus, window, |this, window, cx| {
                this.spy_from_composer(window, cx)
            }),
        ];
        Self {
            focus,
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
            _focus: subs,
        }
    }
}

impl Shell {
    fn composer_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        // The completion popup owns these keys while it is open.
        enum CompKey {
            Up,
            Down,
            Accept,
            Close,
        }
        let prompt_key = match &self.composer.completion {
            Some(c) if c.prompts && k.modifiers.alt && !k.modifiers.control => match k.key.as_str()
            {
                "p" => c
                    .items
                    .get(c.selected)
                    .and_then(|i| i.prompt)
                    .map(|i| (i, true)),
                "d" => c
                    .items
                    .get(c.selected)
                    .and_then(|i| i.prompt)
                    .map(|i| (i, false)),
                _ => None,
            },
            _ => None,
        };
        if let Some((i, pin)) = prompt_key {
            cx.stop_propagation();
            if pin {
                self.saved_prompts[i].pinned = !self.saved_prompts[i].pinned;
            } else {
                let gone = self.saved_prompts.remove(i);
                self.composer.flash = Some(format!("Deleted !{}", gone.name).into());
            }
            crate::prompts::store(&self.saved_prompts);
            let selected = self.composer.completion.as_ref().map_or(0, |c| c.selected);
            self.update_completion(cx);
            if let Some(c) = &mut self.composer.completion {
                c.selected = selected.min(c.items.len().saturating_sub(1));
            }
            cx.notify();
            return;
        }
        let comp_key = if self.composer.completion.is_some() {
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
                    if let Some(c) = &mut self.composer.completion {
                        c.selected = c
                            .selected
                            .checked_sub(1)
                            .unwrap_or(c.items.len().saturating_sub(1));
                    }
                }
                CompKey::Down => {
                    if let Some(c) = &mut self.composer.completion {
                        c.selected = (c.selected + 1) % c.items.len().max(1);
                    }
                }
                CompKey::Accept => self.accept_completion(cx),
                CompKey::Close => self.composer.completion = None,
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
            self.cycle_permissions(cx);
            cx.notify();
            return;
        }
        if self.composer.flash.take().is_some() {
            cx.notify();
        }
        if ctrl_only && !m.shift && k.key == "enter" {
            cx.stop_propagation();
            self.composer.completion = None;
            self.submit_with(true, window, cx);
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
            let draft = &mut self.env_mut().draft;
            if if redo { draft.redo() } else { draft.undo() } {
                self.composer_changed(cx);
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
                0 => self.paste_into_composer(cx.read_from_clipboard(), cx),
                1 => self.copy_composer(false, cx),
                2 => self.copy_composer(true, cx),
                _ => {
                    self.env_mut().draft.select_all();
                    self.composer_to_primary(cx);
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
                self.env_mut().draft.home(true);
                ("backspace", false)
            }
            (_, key) => (key, crate::platform::word_key(m)),
        };
        if !matches!(key, "up" | "down") {
            self.composer.goal_x = None;
        }
        let extend = m.shift;
        if matches!(key, "up" | "down") && !word && !extend && self.recall(key == "up") {
            cx.stop_propagation();
            self.composer_changed(cx);
            self.composer.completion = None;
            cx.notify();
            return;
        }
        let draft = &mut self.env_mut().draft;
        match key {
            "left" => draft.left(word, extend),
            "right" => draft.right(word, extend),
            "home" if word => draft.move_to(0, extend),
            "home" => draft.home(extend),
            "end" if word => draft.move_to(usize::MAX, extend),
            "end" => draft.end(extend),
            "up" | "down" if !word => self.composer_vertical(key == "up", extend),
            "backspace" => draft.backspace(word),
            "delete" => draft.delete(word),
            "enter" if extend => draft.insert("\n"),
            "enter" => self.submit(window, cx),
            "escape" if draft.has_selection() => {
                let head = draft.head();
                draft.move_to(head, false);
            }
            "escape" => {
                if !self.cancel_active(cx) {
                    window.focus(&self.focus, cx)
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        self.composer_changed(cx);
        self.update_completion(cx);
        cx.notify();
    }

    /// After an edit or a caret move: show the caret, and save the draft once
    /// typing pauses.
    pub(super) fn composer_changed(&mut self, cx: &mut Context<Self>) {
        // The raccoon watches you type: how far along its line the caret is.
        let draft = &self.env().draft;
        let caret = (!draft.is_empty()).then(|| {
            let head = draft.head().min(draft.len());
            let line = draft[..head].rfind('\n').map_or(0, |i| i + 1);
            (draft[line..head].chars().count() % 40) as f32 / 40.
        });
        if let (Some(caret), Some(m)) = (caret, self.composer_mascot(cx)) {
            m.update(cx, |m, cx| {
                m.key(caret);
                cx.notify();
            });
        }
        self.hold_caret();
        self.composer.reveal.set(true);
        self.composer.draft_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_SAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| this.save(cx));
        }));
    }

    /// Input shows the caret and keeps it still until the next blink.
    fn hold_caret(&mut self) {
        self.composer.caret.borrow_mut().hold();
    }

    /// Up in an empty composer brings back the last prompt sent, and more Ups
    /// older ones; Down walks back toward an empty composer. Only while the
    /// recalled text is untouched and the caret is on its first line (Up) or
    /// last line (Down).
    fn recall(&mut self, up: bool) -> bool {
        let draft = &self.env().draft;
        let showing = self
            .composer
            .history_at
            .filter(|&i| self.composer.history.get(i) == Some(&**draft));
        let (before, after) = draft.split_at(draft.head());
        let edge = if up {
            !before.contains('\n')
        } else {
            !after.contains('\n')
        };
        let next = match showing {
            None if up && draft.is_empty() => match self.composer.history.older(None) {
                Some(i) => Some(i),
                None => return false,
            },
            Some(i) if edge && up => Some(self.composer.history.older(Some(i)).unwrap_or(i)),
            Some(i) if edge => self.composer.history.newer(i),
            _ => return false,
        };
        self.composer.history_at = next;
        let text = next
            .and_then(|i| self.composer.history.get(i))
            .unwrap_or("")
            .to_string();
        self.env_mut().draft.set(text);
        true
    }

    /// Up/Down in the composer: the line above or below at the same column,
    /// or the start or end of the draft past its first or last line.
    fn composer_vertical(&mut self, up: bool, extend: bool) {
        let layout = self.composer.layout.borrow().clone();
        let head = self.env().draft.head();
        let target = match layout
            .as_ref()
            .and_then(|l| Some((l, l.position_for_index(head)?)))
        {
            Some((layout, at)) => {
                let x = *self.composer.goal_x.get_or_insert(at.x);
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
        self.env_mut().draft.move_to(target, extend);
    }

    /// The draft index under `position`, from the text as last painted.
    fn composer_index_at(&self, position: gpui::Point<Pixels>) -> Option<usize> {
        let layout = self.composer.layout.borrow().clone()?;
        Some(layout.index_for_position(position).unwrap_or_else(|ix| ix))
    }

    /// A click in the composer's text: places the caret (Shift extends the
    /// selection), a double click takes the word, a triple click the line.
    fn composer_click(
        &mut self,
        position: gpui::Point<Pixels>,
        clicks: usize,
        shift: bool,
        cx: &mut Context<Self>,
    ) {
        self.composer.goal_x = None;
        let Some(ix) = self.composer_index_at(position) else {
            return;
        };
        self.hold_caret();
        let draft = &mut self.env_mut().draft;
        match clicks {
            0 | 1 => draft.move_to(ix, shift),
            2 => draft.select(draft.word_at(ix)),
            _ => draft.select(draft.line_at(ix)),
        }
        self.composer.selecting = true;
        self.update_completion(cx);
        cx.notify();
    }

    fn composer_drag(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(ix) = self.composer_index_at(position) {
            self.env_mut().draft.move_to(ix, true);
            self.composer_changed(cx);
            cx.notify();
        }
    }

    fn composer_release(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.composer.selecting) {
            self.composer_to_primary(cx);
            self.update_completion(cx);
        }
    }

    /// Linux's primary selection: what is selected can be pasted elsewhere with a middle click.
    fn composer_to_primary(&self, cx: &mut Context<Self>) {
        let draft = &self.env().draft;
        if draft.has_selection() {
            crate::platform::write_primary(
                cx,
                ClipboardItem::new_string(draft.selected_text().to_string()),
            );
        }
    }

    // ---- completion: `@` files and `/` commands ----------------------------

    /// The files of the visible environment for `@` completion, loaded in the
    /// background on the first ask (or once stale).
    fn file_index(&mut self, cx: &mut Context<Self>) -> Option<Arc<Vec<String>>> {
        let env_path = self.env().path.clone();
        if let Some((path, at, files)) = &self.composer.file_index
            && *path == env_path
            && at.elapsed() < FILE_INDEX_TTL
        {
            return Some(files.clone());
        }
        // A server's environment: its files are listed over there, and
        // the answer fills the index (`server_files_message`).
        if crate::model::Remote::is_path(&env_path) {
            if self.composer.file_index_loading.as_ref() != Some(&env_path)
                && self.server_list_files(&env_path)
            {
                self.composer.file_index_loading = Some(env_path);
            }
            return None;
        }
        if self.composer.file_index_loading.as_ref() != Some(&env_path) {
            self.composer.file_index_loading = Some(env_path.clone());
            cx.spawn(async move |this, cx| {
                let p = env_path.clone();
                let files = cx
                    .background_spawn(crate::guard::task(async move {
                        git::ls_files(&p).unwrap_or_default()
                    }))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if this.composer.file_index_loading.as_ref() == Some(&env_path) {
                        this.composer.file_index_loading = None;
                    }
                    this.composer.file_index =
                        Some((env_path.clone(), Instant::now(), Arc::new(files)));
                    // The popup may have been waiting for the index.
                    this.update_completion(cx);
                });
            })
            .detach();
        }
        None
    }

    /// The skills of the visible environment for `/` completion, read in the
    /// background on the first ask (or once stale). Claude Code and Workers AI
    /// read them, so they show before a session has said what it offers.
    /// With `mirrored`, for a server's environment, only what this computer
    /// mirrors to servers: the rest is not there.
    fn skill_index(
        &mut self,
        mirrored: bool,
        cx: &mut Context<Self>,
    ) -> Option<Arc<Vec<crate::agent::Choice>>> {
        let env_path = self.env().path.clone();
        if let Some((path, at, skills)) = &self.composer.skill_index
            && *path == env_path
            && at.elapsed() < FILE_INDEX_TTL
        {
            return Some(skills.clone());
        }
        if !self.composer.skill_index_loading {
            self.composer.skill_index_loading = true;
            let for_path = env_path.clone();
            cx.spawn(async move |this, cx| {
                let p = for_path.clone();
                let skills = cx
                    .background_spawn(crate::guard::task(async move {
                        if mirrored {
                            crate::agent::mirror::mirrored_commands()
                        } else {
                            crate::agent::claude_ext::skill_choices(&p)
                        }
                    }))
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.composer.skill_index_loading = false;
                    this.composer.skill_index = Some((for_path, Instant::now(), Arc::new(skills)));
                    this.update_completion(cx);
                });
            })
            .detach();
        }
        // While they are read again, the last ones read for this environment.
        self.composer
            .skill_index
            .as_ref()
            .filter(|(p, ..)| *p == env_path)
            .map(|(_, _, s)| s.clone())
    }

    pub(super) fn update_completion(&mut self, cx: &mut Context<Self>) {
        let was_open = self.composer.completion.is_some();
        self.composer.completion = self.compute_completion(cx);
        if was_open || self.composer.completion.is_some() {
            cx.notify();
        }
    }

    fn compute_completion(&mut self, cx: &mut Context<Self>) -> Option<Completion> {
        if !self.has_projects() {
            return None;
        }
        let draft = self.env().draft.bare();
        let token_end = draft.head();
        // `!` starting the draft: saved prompts, the rest of the line filtering them.
        if let Some(query) = draft.strip_prefix('!')
            && !query.contains('\n')
        {
            let mut items: Vec<CompletionItem> = crate::prompts::ranked(&self.saved_prompts, query)
                .into_iter()
                .take(COMPLETION_ROWS)
                .map(|i| {
                    let p = &self.saved_prompts[i];
                    let first = p
                        .text
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .trim();
                    CompletionItem {
                        label: format!("{}!{}", if p.pinned { "★ " } else { "" }, p.name).into(),
                        detail: Some(first.to_string().into()),
                        insert: p.text.clone(),
                        prompt: Some(i),
                    }
                })
                .collect();
            if items.is_empty() {
                items.push(CompletionItem {
                    label: if self.saved_prompts.is_empty() {
                        "No saved prompts yet"
                    } else {
                        "No prompt matches"
                    }
                    .into(),
                    detail: Some(
                        crate::platform::shortcut_label(
                            "Write a prompt, then Ctrl+S saves it for !",
                        )
                        .into(),
                    ),
                    insert: draft.to_string(),
                    prompt: None,
                });
            }
            return Some(Completion {
                token_start: 0,
                token_end: draft.len(),
                items,
                selected: 0,
                prompts: true,
            });
        }
        let (token_start, token) = draft.token()?;
        if token.is_empty() {
            return None;
        }
        // `/cmd` as the only token: the agent's own reported commands.
        if token_start == 0
            && let Some(name) = token.strip_prefix('/')
        {
            if name.contains(char::is_whitespace) {
                return None;
            }
            let thread = self.active_thread(cx);
            // This computer's skills, where the agent sees them.
            let session = match self.strip.tabs.get(self.composer_tab()).map(|t| &t.kind) {
                Some(TabKind::Session { id, .. }) => Some(id.as_ref()),
                _ => None,
            };
            let sandboxed = thread.as_ref().map(|t| t.read(cx).sandbox().is_some());
            // An empty list the server sent still counts: it has none.
            let none_reported = thread.as_ref().is_none_or(|t| !t.read(cx).commands_known());
            let on_server = self.env().remote.is_some() || self.project().is_remote();
            let claude_like = matches!(
                self.composer_agent(),
                AgentKind::Claude | AgentKind::WorkersAi
            );
            let skills = if on_server {
                // Until the server says what its session has, what this
                // computer mirrored there.
                (claude_like && none_reported)
                    .then(|| self.skill_index(true, cx))
                    .flatten()
            } else {
                (claude_like && host_skills_reach(self.project(), self.env(), session, sandboxed))
                    .then(|| self.skill_index(false, cx))
                    .flatten()
            };
            let reported: &[crate::agent::Choice] = match &thread {
                Some(t) => t.read(cx).commands(),
                None => &[],
            };
            let name = name.to_lowercase();
            // `/stage` needs a session to know its files; an agent's own `stage` wins.
            let stage =
                (thread.is_some() && !reported.iter().any(|c| c.name == "stage")).then(|| {
                    crate::agent::Choice {
                        id: "stage".into(),
                        name: "stage".into(),
                        description: Some("Stage only the files this session changed".into()),
                    }
                });
            // A skill the session reported already shows once.
            let mut seen = std::collections::HashSet::new();
            let items: Vec<CompletionItem> = stage
                .iter()
                .chain(reported)
                .chain(skills.iter().flat_map(|s| s.iter()))
                .filter(|c| seen.insert(c.name.to_lowercase()))
                .filter(|c| name.is_empty() || c.name.to_lowercase().starts_with(&name))
                .take(COMPLETION_ROWS)
                .map(|c| CompletionItem {
                    label: format!("/{}", c.name).into(),
                    detail: c.description.clone().map(SharedString::from),
                    insert: format!("/{} ", c.name),
                    prompt: None,
                })
                .collect();
            return (!items.is_empty()).then_some(Completion {
                token_start,
                token_end,
                items,
                selected: 0,
                prompts: false,
            });
        }
        // `@path` in any token: the environment's files.
        if let Some(prefix) = token.strip_prefix('@') {
            let prefix = prefix.to_lowercase();
            let files = self.file_index(cx)?;
            let mut scored: Vec<(u8, usize, &String)> = files
                .iter()
                .filter_map(|f| {
                    if prefix.is_empty() {
                        return Some((2, f.len(), f));
                    }
                    let lower = f.to_lowercase();
                    let name = f.rsplit('/').next().unwrap_or(f);
                    if name.to_lowercase().starts_with(&prefix) {
                        Some((0, f.len(), f))
                    } else if lower.contains(&prefix) {
                        Some((1, f.len(), f))
                    } else {
                        None
                    }
                })
                .collect();
            scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then_with(|| a.2.cmp(b.2)));
            let items: Vec<CompletionItem> = scored
                .into_iter()
                .take(COMPLETION_ROWS)
                .map(|(_, _, f)| CompletionItem {
                    label: f.clone().into(),
                    detail: None,
                    insert: format!("@{f} "),
                    prompt: None,
                })
                .collect();
            return (!items.is_empty()).then_some(Completion {
                token_start,
                token_end,
                items,
                selected: 0,
                prompts: false,
            });
        }
        None
    }

    fn accept_completion(&mut self, cx: &mut Context<Self>) {
        let Some(comp) = self.composer.completion.take() else {
            return;
        };
        let Some(item) = comp.items.get(comp.selected) else {
            return;
        };
        if let Some(p) = item.prompt.and_then(|i| self.saved_prompts.get_mut(i)) {
            p.uses += 1;
            p.last_used = model::unix_now();
            crate::prompts::store(&self.saved_prompts);
        }
        let draft = &mut self.env_mut().draft;
        // Completing in the middle of the text, before a space: no second space.
        let spaced = draft[comp.token_end.min(draft.len())..].starts_with(char::is_whitespace);
        let insert = if spaced {
            item.insert.trim_end_matches(' ')
        } else {
            item.insert.as_str()
        };
        draft.replace(comp.token_start..comp.token_end, insert);
        self.composer_changed(cx);
        cx.notify();
    }

    /// Opens arakun's file picker; the picked files go into the draft as `@`
    /// mentions, and images among them are sent along with the prompt.
    fn attach_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.composer.completion = None;
        let env_path = self.env().path.clone();
        let picker = cx.new(|cx| AttachPicker::new(env_path.clone(), window, cx));
        let sub = cx.subscribe_in(
            &picker,
            window,
            move |this, _, ev: &AttachEvent, window, cx| {
                if let AttachEvent::Attach(paths) = ev
                    && this.env().path == env_path
                {
                    this.push_mentions(paths);
                    this.composer_changed(cx);
                }
                this.close_modal(window, cx);
            },
        );
        self.modal = Some((Modal::Attach(picker), sub));
        cx.notify();
    }

    /// Keeps the draft as a saved prompt, named after its first words.
    fn save_draft_as_prompt(&mut self, cx: &mut Context<Self>) {
        let text = self.env().draft.trim().to_string();
        if text.is_empty() || text.starts_with('!') {
            self.composer.flash = Some(
                crate::platform::shortcut_label("Write a prompt first; Ctrl+S saves it for !")
                    .into(),
            );
        } else if let Some(p) = self.saved_prompts.iter().find(|p| p.text == text) {
            self.composer.flash = Some(format!("Already saved as !{}", p.name).into());
        } else {
            let name = crate::prompts::name_for(&text, &self.saved_prompts);
            self.saved_prompts.push(crate::prompts::SavedPrompt {
                name: name.clone(),
                text,
                pinned: false,
                uses: 0,
                last_used: model::unix_now(),
            });
            crate::prompts::store(&self.saved_prompts);
            self.composer.flash = Some(format!("Saved as !{name}. Type ! to use it.").into());
        }
        cx.notify();
    }

    pub(super) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.submit_with(false, window, cx)
    }

    /// Sends the draft; with `now`, a running turn is stopped for it instead of
    /// the draft waiting in the queue.
    fn submit_with(&mut self, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        let env_path = self.env().path.clone();
        // A dropped session still being read would miss this prompt.
        if self.dropping.contains_key(&env_path) {
            self.composer.flash =
                Some("Still reading the dropped session. Send again in a moment.".into());
            cx.notify();
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
        let mut text = self.env().draft.trim().to_string();
        let typed = text.clone();
        let pastes = self.pastes.remove(&env_path).unwrap_or_default();
        let notes = self.notes.get(&env_path).cloned().unwrap_or_default();
        let contexts = self.attached.remove(&env_path).unwrap_or_default();
        let images = std::mem::take(&mut self.env_mut().draft_images);
        if text.is_empty()
            && pastes.is_empty()
            && notes.is_empty()
            && contexts.is_empty()
            && images.is_empty()
        {
            return;
        }
        if !images.is_empty() {
            // Mentioned like any file: the agent gets the pictures and their paths.
            let mentions: Vec<String> = images
                .iter()
                .map(|p| {
                    crate::agent::attach::mention(
                        &p.strip_prefix(&env_path).unwrap_or(p).to_string_lossy(),
                    )
                })
                .collect();
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&mentions.join(" "));
        }
        for c in &contexts {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&c.block);
        }
        for p in &pastes {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&crate::agent::attach::wrap_paste(&p.text));
        }
        if !notes.is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&crate::agent::notes::format(&notes));
            self.notes.remove(&env_path);
            self.sync_note_marks(&env_path, cx);
        }
        self.env_mut().draft.clear();
        self.composer_changed(cx);
        if let Some(m) = self.composer_mascot(cx) {
            m.update(cx, |m, cx| {
                m.sent();
                cx.notify();
            });
        }
        self.composer.history_at = None;
        if self.composer.history.push(&typed) {
            let bytes = self.composer.history.bytes();
            cx.background_spawn(crate::guard::task(
                async move { crate::history::write(&bytes) },
            ))
            .detach();
        }
        self.send_prompt(text, now, window, cx);
    }

    /// The tab the composer writes to: the active tab, or the session showing beside it
    /// while the other pane (a page, a file) has the focus.
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
        // another environment in the other pane does not take it.
        let here = |i: usize| {
            self.try_env()
                .is_some_and(|e| strip.tabs.get(i).is_some_and(|t| t.is_in(&e.path)))
        };
        match self.split_other() {
            Some(other) if !session(active) && !fresh && session(other) && here(other) => other,
            _ => active,
        }
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
        } else if let Some(split) = self.strip.split.filter(|_| window.focused(cx).is_some()) {
            match (split.dir, split.focus_first) {
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

    pub(super) fn render_composer(
        &self,
        empty: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
        let in_session = session_id.is_some();
        let busy = thread.as_ref().is_some_and(|t| t.read(cx).is_busy());
        use crate::agent::Setting;
        let model_label = self.setting_value_label(Setting::Model, cx);
        // "Default" effort is whatever the model defaults to; it says nothing on the chip.
        let effort_label = self
            .setting_value_label(Setting::Effort, cx)
            .filter(|l| !l.is_empty() && l != "Default");
        let fast = self
            .setting_choices(Setting::Fast, cx)
            .is_some_and(|(current, _)| current == "on");
        let permissions = self.permissions_label(cx);
        let usage_line = thread.as_ref().and_then(|t| t.read(cx).usage().summary());
        // The Claude seat the session runs on, while it runs (`agent::seats`).
        let seat = thread
            .as_ref()
            .and_then(|t| t.read(cx).seat().map(crate::agent::seats::label))
            .map(|s| crate::redact::value(&s));
        let focused = self.composer.focus.is_focused(window);
        let entity = cx.entity();
        let focus = self.composer.focus.clone();
        let draft = env.draft.bare();
        let pastes: Vec<(usize, usize)> = self
            .pastes
            .get(&env.path)
            .map(|v| v.iter().map(|p| (p.lines, p.chars)).collect())
            .unwrap_or_default();
        let paste_row: Option<AnyElement> = (!pastes.is_empty()).then(|| {
            div()
                .flex()
                .flex_wrap()
                .gap(crate::ui::sp(6.))
                .px(crate::ui::sp(12.))
                .pt(crate::ui::sp(10.))
                .children(pastes.iter().enumerate().map(|(i, &(lines, chars))| {
                    div()
                        .id(("paste-chip", i))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .h(px(26.))
                        .pl(crate::ui::sp(8.))
                        .pr(crate::ui::sp(4.))
                        .rounded(crate::ui::rad(8.))
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::fill_hover())
                        .text_size(px(ui::TEXT_SM))
                        .child(icon("file01", 12.).text_color(theme::text_subtle()))
                        .child(div().text_color(theme::text_muted()).child("Pasted text"))
                        .child(div().text_color(theme::text_subtle()).child(format!(
                            "{lines} lines · {:.1}k chars",
                            chars as f32 / 1000.
                        )))
                        .child(
                            div()
                                .id(("paste-remove", i))
                                .size(px(18.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(crate::ui::rad(4.))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::fill_selected()))
                                .child(icon("cancel01", 11.).text_color(theme::text_subtle()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let path = this.env().path.clone();
                                    if let Some(v) = this.pastes.get_mut(&path) {
                                        if i < v.len() {
                                            v.remove(i);
                                        }
                                        if v.is_empty() {
                                            this.pastes.remove(&path);
                                        }
                                    }
                                    cx.notify();
                                })),
                        )
                }))
                .into_any_element()
        });

        let has_notes = self.notes.get(&env.path).is_some_and(|v| !v.is_empty());
        let note_row = self.render_note_row(cx);
        let has_context = self.attached.get(&env.path).is_some_and(|v| !v.is_empty());
        let context_row = self.render_context_row(cx);
        let image_row = self.render_image_row(cx);
        let has_images = image_row.is_some();

        // The `@`/`/` completion popup sits right above the text line.
        let comp_popup: Option<AnyElement> = self.composer.completion.as_ref().map(|comp| {
            div()
                .id("completion")
                .flex()
                .flex_col()
                .mx(px(8.))
                .mt(px(8.))
                .py(crate::ui::sp(4.))
                .rounded(crate::ui::rad(12.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::fill_hover())
                .children(comp.items.iter().enumerate().map(|(i, item)| {
                    let selected = i == comp.selected;
                    div()
                        .id(("compl", i))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .h(px(26.))
                        .px(crate::ui::sp(10.))
                        .rounded(crate::ui::rad(8.))
                        .cursor_pointer()
                        .when(selected, |d| d.bg(theme::selection()))
                        .child(
                            div()
                                .flex_none()
                                .font_family(theme::FONT_MONO)
                                .text_size(px(ui::TEXT_SM))
                                .text_color(if selected { theme::content() } else { theme::mix(0.75) })
                                .child(item.label.clone()),
                        )
                        .when_some(item.detail.clone(), |d, det| {
                            d.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(ui::TEXT_XS))
                                    .text_color(theme::text_subtle())
                                    .child(det),
                            )
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(c) = &mut this.composer.completion {
                                c.selected = i;
                            }
                            this.accept_completion(cx);
                        }))
                }))
                .when(comp.prompts, |d| {
                    d.child(
                        div()
                            .px(crate::ui::sp(10.))
                            .pt(crate::ui::sp(4.))
                            .pb(crate::ui::sp(2.))
                            .mt(px(2.))
                            .border_t_1()
                            .border_color(theme::border_subtle())
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(crate::platform::shortcut_label("Enter inserts · Alt+P pins · Alt+D deletes · Ctrl+S saves the draft")),
                    )
                })
                .into_any_element()
        });

        // The caret is painted at the end of the laid-out draft rather than typed
        // into it as a glyph, which sat off the last letter by the glyph's side
        // bearings and could wrap onto a line of its own.
        let mut painted: Option<(TextLayout, usize)> = None;
        let mut show_caret = focused;
        let caret = self.composer.caret.clone();
        if draft.is_empty() {
            self.composer.layout.borrow_mut().take();
        }
        let (layout_cell, reveal, scroll) = (
            self.composer.layout.clone(),
            self.composer.reveal.clone(),
            self.composer.scroll.clone(),
        );
        let text: AnyElement = if draft.is_empty() {
            div()
                .relative()
                .text_color(theme::text_faint())
                .child(PLACEHOLDER)
                .into_any_element()
        } else {
            let sel = draft.selection();
            // Highlights may not overlap; while composing, the input method's
            // text is underlined and any selection inside it is its own.
            let highlight = match draft.marked() {
                Some(marked) => Some((
                    marked,
                    HighlightStyle {
                        underline: Some(UnderlineStyle {
                            thickness: px(1.),
                            color: Some(theme::content()),
                            wavy: false,
                        }),
                        ..Default::default()
                    },
                )),
                None => (!sel.is_empty()).then(|| {
                    let alpha = if focused { 0.35 } else { 0.18 };
                    (
                        sel.clone(),
                        HighlightStyle {
                            background_color: Some(gpui::Hsla {
                                a: alpha,
                                ..theme::text_selection()
                            }),
                            ..Default::default()
                        },
                    )
                }),
            };
            let styled = StyledText::new(draft.to_string()).with_highlights(highlight);
            painted = Some((styled.layout().clone(), draft.head()));
            show_caret = focused && sel.is_empty();
            styled.into_any_element()
        };

        div()
            .id("composer")
            .track_focus(&self.composer.focus)
            .on_key_down(cx.listener(Self::composer_key))
            .drag_over::<drag::DraggedSession>(|s, _, _, _| {
                s.border_color(theme::active())
                    .bg(theme::active().opacity(0.06))
            })
            .on_drop(cx.listener(|this, d: &drag::DraggedSession, window, cx| {
                this.drop_session_on_composer(d, window, cx)
            }))
            // Files dropped from a file manager: images as pictures, the rest as mentions.
            .drag_over::<gpui::ExternalPaths>(|s, _, _, _| {
                s.border_color(theme::active())
                    .bg(theme::active().opacity(0.06))
            })
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.push_mentions(paths.paths());
                    window.focus(&this.composer.focus, cx);
                    this.composer_changed(cx);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.composer.focus, cx);
                    cx.notify();
                }),
            )
            // Linux middle-click pastes the primary selection.
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.composer.focus, cx);
                    this.paste_into_composer(crate::platform::read_primary(cx), cx);
                }),
            )
            .cursor_text()
            .flex()
            .flex_col()
            .w_full()
            // As round as the window's cards; its buttons sit 8 px in. Raised
            // off the sheet by its tone; the border only shows while typing
            // (and lights up when something is dragged over).
            .rounded(crate::ui::rad(ui::RADIUS_XL))
            .border_1()
            .border_color(if focused {
                theme::border()
            } else {
                gpui::transparent_black()
            })
            .bg(theme::raised())
            .when_some(comp_popup, |d, p| d.child(p))
            .when_some(image_row, |d, p| d.child(p))
            .when_some(paste_row, |d, p| d.child(p))
            .when_some(note_row, |d, p| d.child(p))
            .when_some(context_row, |d, p| d.child(p))
            .when_some(self.agent_out_of_limits(agent, cx), |d, why| {
                let other = if agent == AgentKind::Droid {
                    AgentKind::Claude
                } else {
                    AgentKind::Droid
                };
                let other_ok = self.agent_out_of_limits(other, cx).is_none();
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .mx(px(8.))
                        .mt(px(8.))
                        .px(crate::ui::sp(10.))
                        .py(crate::ui::sp(6.))
                        .rounded(crate::ui::rad(ui::RADIUS))
                        .bg(Hsla::from(theme::attention()).opacity(0.10))
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_muted())
                        .child(icon("alert-circle", 14.).text_color(theme::attention()))
                        .child(div().flex_1().min_w_0().child(why))
                        .when(!in_session && other_ok, |d| {
                            d.child(
                                ui::button(
                                    "composer-switch-agent",
                                    format!("Use {}", other.label()),
                                    ui::ButtonKind::Outline,
                                    true,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.env_mut().agent = other;
                                        model::set_default_agent(other);
                                        this.save(cx);
                                        cx.notify();
                                    },
                                )),
                            )
                        }),
                )
            })
            .when_some(self.composer.flash.clone(), |d, f| {
                d.child(
                    div()
                        .px(crate::ui::sp(12.))
                        .pt(crate::ui::sp(8.))
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_subtle())
                        .child(f),
                )
            })
            .child(
                div()
                    .id("composer-text")
                    .relative()
                    .px(crate::ui::sp(12.))
                    .pt(crate::ui::sp(if empty { 12. } else { COMPOSER_PAD }))
                    .pb(crate::ui::sp(if empty { 16. } else { COMPOSER_PAD }))
                    .text_size(px(ui::TEXT_LG))
                    .line_height(px(22.))
                    .max_h(px(160.))
                    .overflow_y_scroll()
                    .track_scroll(&self.composer.scroll)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                            window.focus(&this.composer.focus, cx);
                            this.composer_click(
                                ev.position,
                                ev.click_count,
                                ev.modifiers.shift,
                                cx,
                            );
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                        if this.composer.selecting && ev.pressed_button == Some(MouseButton::Left) {
                            this.composer_drag(ev.position, cx);
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.composer_release(cx)),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.composer_release(cx)),
                    )
                    // The caret and input canvas cover the text only: sized to the
                    // scroll box itself, the bottom padding would count as overflow
                    // and the composer would scroll even when empty.
                    .child(
                        div().relative().child(text).child(
                            canvas(
                                |_, _, _| {},
                                move |bounds, _, window, cx| {
                                    window.handle_input(
                                        &focus,
                                        ElementInputHandler::new(bounds, entity.clone()),
                                        cx,
                                    );
                                    // The text is laid out and placed by now: keep it for
                                    // clicks and the IME, paint the caret, and bring the
                                    // caret into view after an edit.
                                    let (at, line) = match &painted {
                                        Some((layout, head)) => {
                                            *layout_cell.borrow_mut() = Some(layout.clone());
                                            let Some(at) = layout.position_for_index(*head) else {
                                                caret.borrow_mut().hide();
                                                return;
                                            };
                                            (at, layout.line_height())
                                        }
                                        // Empty: just before the placeholder.
                                        None => (
                                            bounds.origin + point(px(-1.), px(0.)),
                                            px(PLACEHOLDER_LINE),
                                        ),
                                    };
                                    if show_caret {
                                        let (drawn, alpha) = caret.borrow_mut().frame(
                                            at - bounds.origin,
                                            window,
                                            cx,
                                        );
                                        let drawn = bounds.origin + drawn;
                                        window.paint_quad(
                                            fill(
                                                Bounds::new(
                                                    point(
                                                        drawn.x,
                                                        drawn.y + (line - px(CARET_HEIGHT)) / 2.,
                                                    ),
                                                    size(px(CARET_WIDTH), px(CARET_HEIGHT)),
                                                ),
                                                crate::caret::faded(theme::active(), alpha),
                                            )
                                            .corner_radii(crate::caret::radius(CARET_WIDTH)),
                                        );
                                    } else {
                                        caret.borrow_mut().hide();
                                    }
                                    if painted.is_none() {
                                        return;
                                    }
                                    if reveal.take() {
                                        let view = scroll.bounds();
                                        let (top, bottom) = (
                                            view.top() + px(COMPOSER_PAD),
                                            view.bottom() - px(COMPOSER_PAD),
                                        );
                                        let mut offset = scroll.offset();
                                        let shift = if at.y < top {
                                            top - at.y
                                        } else if at.y + line > bottom {
                                            bottom - (at.y + line)
                                        } else {
                                            px(0.)
                                        };
                                        if shift != px(0.) {
                                            offset.y = (offset.y + shift)
                                                .clamp(-scroll.max_offset().y, px(0.));
                                            scroll.set_offset(offset);
                                            // A refresh asked for while drawing is dropped.
                                            window.on_next_frame(|window, _| window.refresh());
                                        }
                                    }
                                },
                            )
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full(),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    // With the chips wrapped, + and send sit on their last line.
                    .items_end()
                    .gap(crate::ui::sp(4.))
                    .px(crate::ui::sp(8.))
                    .pb(crate::ui::sp(8.))
                    .child(
                        // Opens the attach picker; picked files become `@` mentions.
                        div()
                            .id("composer-add")
                            .group("composer-add")
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(26.))
                            .rounded(ui::nested(ui::RADIUS_XL, 8.))
                            .bg(theme::selection())
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::selection_hover()))
                            .tooltip(ui::tip("Attach files"))
                            .child(hover_icon(
                                "add01",
                                14.,
                                theme::text_subtle(),
                                theme::content(),
                                "composer-add",
                            ))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.attach_files(window, cx)),
                            ),
                    )
                    // Narrow panes wrap the chips onto a second line rather than push the
                    // send button out of the composer.
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .flex_wrap()
                            .items_center()
                            .gap(crate::ui::sp(4.))
                            .child(
                                // One chip for the agent, its model and reasoning.
                                chip(
                                    "composer-model",
                                    Some(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(crate::ui::sp(3.))
                                            .child(agent.icon(14.))
                                            .when(fast, |d| {
                                                d.child(
                                                    icon("flash", 12.)
                                                        .text_color(theme::text_subtle()),
                                                )
                                            })
                                            .into_any_element(),
                                    ),
                                    model_label.unwrap_or_else(|| agent.label().into()),
                                    effort_label,
                                    true,
                                )
                                .tooltip(ui::tip("Agent, model and reasoning"))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(cx.listener(
                                    |this, ev: &gpui::ClickEvent, _, cx| {
                                        this.open_composer_menu(
                                            MenuTarget::ModelMenu,
                                            ev.position(),
                                            cx,
                                        )
                                    },
                                )),
                            )
                            .when_some(seat, |d, seat| {
                                d.child(
                                    div()
                                        .id("composer-seat")
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .gap(crate::ui::sp(4.))
                                        .px(crate::ui::sp(6.))
                                        .text_size(px(ui::TEXT_XS))
                                        .text_color(theme::text_subtle())
                                        .child(icon("user", 11.).text_color(theme::text_faint()))
                                        .child(seat)
                                        .tooltip(ui::tip(
                                            "The Claude seat this session runs on, for its whole life",
                                        )),
                                )
                            })
                            .when_some(permissions, |d, (label, risky)| {
                                d.child(
                                    chip(
                                        "composer-permissions",
                                        Some(
                                            icon(
                                                if risky {
                                                    "shield-alert"
                                                } else {
                                                    "square-lock02"
                                                },
                                                12.,
                                            )
                                            .text_color(if risky {
                                                theme::warn().into()
                                            } else {
                                                theme::mix(0.5)
                                            })
                                            .into_any_element(),
                                        ),
                                        label,
                                        None,
                                        true,
                                    )
                                    .tooltip(ui::tip("Permissions · Shift+Tab to switch"))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(cx.listener(
                                        |this, ev: &gpui::ClickEvent, _, cx| {
                                            this.open_composer_menu(
                                                MenuTarget::PermissionMenu,
                                                ev.position(),
                                                cx,
                                            )
                                        },
                                    )),
                                )
                            }),
                    )
                    .when_some(usage_line, |d, line| {
                        d.child(
                            div()
                                .flex_none()
                                .px(crate::ui::sp(6.))
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_subtle())
                                .child(line),
                        )
                    })
                    .child({
                        let ready = !draft.trim().is_empty()
                            || !pastes.is_empty()
                            || has_notes
                            || has_context
                            || has_images;
                        let stop = busy && !ready;
                        let lit = ready || stop;
                        div()
                            .id("composer-send")
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(26.))
                            .rounded(ui::nested(ui::RADIUS_XL, 8.))
                            .bg(if lit { gpui::white() } else { theme::mix(0.14) })
                            .when(lit, |d| {
                                d.cursor_pointer()
                                    .hover(|s| s.bg(gpui::white().opacity(0.9)))
                            })
                            .child(
                                icon(
                                    if stop { "square" } else { "arrow-up01" },
                                    if stop { 11. } else { 14. },
                                )
                                .text_color(if lit {
                                    gpui::black()
                                } else {
                                    theme::mix(0.5)
                                }),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if stop {
                                    this.cancel_active(cx);
                                } else {
                                    this.submit(window, cx)
                                }
                            }))
                    }),
            )
    }
}

/// The composer speaks UTF-16 offsets with input methods.
impl EntityInputHandler for Shell {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let draft = &self.env().draft;
        let (a, b) = (
            draft.utf16_to_byte(range_utf16.start),
            draft.utf16_to_byte(range_utf16.end),
        );
        let range = a.min(b)..a.max(b);
        actual_range.replace(draft.byte_to_utf16(range.start)..draft.byte_to_utf16(range.end));
        Some(draft[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let draft = &self.env().draft;
        let sel = draft.selection();
        Some(UTF16Selection {
            range: draft.byte_to_utf16(sel.start)..draft.byte_to_utf16(sel.end),
            reversed: draft.reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let draft = &self.env().draft;
        draft
            .marked()
            .map(|m| draft.byte_to_utf16(m.start)..draft.byte_to_utf16(m.end))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.env_mut().draft.unmark();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = &mut self.env_mut().draft;
        match range_utf16 {
            Some(r) => {
                let range = draft.utf16_to_byte(r.start)..draft.utf16_to_byte(r.end);
                draft.replace(range, text);
            }
            None => draft.insert(text),
        }
        self.composer.goal_x = None;
        self.composer_changed(cx);
        self.update_completion(cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = &mut self.env_mut().draft;
        let range = range_utf16.map(|r| draft.utf16_to_byte(r.start)..draft.utf16_to_byte(r.end));
        let selected = new_selected_range_utf16.map(|r| {
            crate::draft::byte_of_utf16(new_text, r.start)
                ..crate::draft::byte_of_utf16(new_text, r.end)
        });
        draft.compose(range, new_text, selected);
        self.composer_changed(cx);
        cx.notify();
    }

    /// Where the input method's candidate window goes: at the caret.
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.composer.layout.borrow().clone();
        let ix = self.env().draft.utf16_to_byte(range_utf16.start);
        Some(
            match layout
                .as_ref()
                .and_then(|l| Some((l.position_for_index(ix)?, l.line_height())))
            {
                Some((at, line)) => Bounds::new(at, size(px(CARET_WIDTH), line)),
                None => element_bounds,
            },
        )
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let ix = self.composer_index_at(point)?;
        Some(self.env().draft.byte_to_utf16(ix))
    }
}

/// Whether the agent of `session` in `env` sees the skills on this computer,
/// so `/` completion may offer them. A server's environment or project, a
/// sandbox and a headless session each see other files: there only the
/// commands the session reports count. `sandboxed` is the running session's
/// own state: the project's switch applies only to sessions started after it.
fn host_skills_reach(
    project: &Project,
    env: &Environment,
    session: Option<&str>,
    sandboxed: Option<bool>,
) -> bool {
    env.remote.is_none()
        && !project.is_remote()
        && !sandboxed.unwrap_or(project.sandbox)
        && !session.is_some_and(crate::headless::shown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Remote;

    #[test]
    fn host_skills_stay_with_agents_on_this_computer() {
        let reach = |p: &Project, session: Option<&str>, sandboxed: Option<bool>| {
            host_skills_reach(p, &p.environments[0], session, sandboxed)
        };
        let mut project =
            crate::state::new_project(std::path::Path::new("/tmp/arakun-skills-test"));
        assert!(reach(&project, None, None));
        assert!(reach(&project, Some("s-local"), Some(false)));

        crate::headless::set_shown("s-headless", true);
        assert!(!reach(&project, Some("s-headless"), None));
        crate::headless::set_shown("s-headless", false);

        // A running session keeps the sandbox it started with.
        assert!(!reach(&project, Some("s-local"), Some(true)));
        project.sandbox = true;
        assert!(!reach(&project, None, None));
        assert!(reach(&project, Some("s-local"), Some(false)));
        project.sandbox = false;

        project.environments[0].remote = Some(Remote {
            server: "s1".into(),
            machine: "box".into(),
            place: "/srv/app".into(),
        });
        assert!(!reach(&project, None, None));

        let remote = crate::state::new_project(&Remote::project_path("s1/app"));
        assert!(!reach(&remote, None, None));
    }
}
