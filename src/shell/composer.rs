//! The session composer: keys, mouse, input methods, completion, history,
//! the blinking caret and how it is drawn.

use super::*;

/// The composer caret, in pixels.
const CARET_WIDTH: f32 = 1.5;
const CARET_HEIGHT: f32 = 16.;
/// The caret stays whole this long after input before it starts to blink.
const CARET_SOLID: Duration = Duration::from_millis(500);
/// One blink: the caret shrinks to its middle and grows back, like VS Code's
/// `expand` cursor.
const CARET_PERIOD: Duration = Duration::from_millis(1000);
/// Blinks before the caret stays still, as GTK's stops after ~10 s idle.
const CARET_BLINKS: u32 = 10;
/// How often the caret is redrawn while it shrinks or grows.
const CARET_FRAME: Duration = Duration::from_millis(16);

/// How much of the caret's height shows after `idle` without input: whole
/// while held, then each half of a blink holds its end for a fifth and eases
/// across the rest, as VS Code's `expand` keyframes do.
fn caret_scale(idle: Duration) -> f32 {
    let Some(t) = idle.checked_sub(CARET_SOLID) else {
        return 1.;
    };
    if t >= CARET_PERIOD * CARET_BLINKS {
        return 1.;
    }
    let p = (t.as_secs_f32() / CARET_PERIOD.as_secs_f32()).fract();
    let half = if p < 0.5 { p * 2. } else { 2. - p * 2. };
    let k = ((half - 0.2) / 0.6).clamp(0., 1.);
    1. - k * k * (3. - 2. * k)
}
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
        let prompt_key = match &self.completion {
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
                self.composer_flash = Some(format!("Deleted !{}", gone.name).into());
            }
            crate::prompts::store(&self.saved_prompts);
            let selected = self.completion.as_ref().map_or(0, |c| c.selected);
            self.update_completion(cx);
            if let Some(c) = &mut self.completion {
                c.selected = selected.min(c.items.len().saturating_sub(1));
            }
            cx.notify();
            return;
        }
        let comp_key = if self.completion.is_some() {
            match k.key.as_str() {
                "up" => Some(CompKey::Up),
                "down" => Some(CompKey::Down),
                "tab" => Some(CompKey::Accept),
                "enter" if !k.modifiers.shift => Some(CompKey::Accept),
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
                        c.selected = c
                            .selected
                            .checked_sub(1)
                            .unwrap_or(c.items.len().saturating_sub(1));
                    }
                }
                CompKey::Down => {
                    if let Some(c) = &mut self.completion {
                        c.selected = (c.selected + 1) % c.items.len().max(1);
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
            self.cycle_permissions(cx);
            cx.notify();
            return;
        }
        if self.composer_flash.take().is_some() {
            cx.notify();
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
            self.composer_goal_x = None;
        }
        let extend = m.shift;
        if matches!(key, "up" | "down") && !word && !extend && self.recall(key == "up") {
            cx.stop_propagation();
            self.composer_changed(cx);
            self.completion = None;
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
        self.composer_reveal.set(true);
        self.draft_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_SAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| this.save(cx));
        }));
    }

    /// Input shows the caret and keeps it still until the next blink.
    fn hold_caret(&mut self) {
        self.caret_scale = 1.;
        self.caret_held = Instant::now();
    }

    /// Blinks the caret while the composer has focus in the active window.
    pub(super) fn sync_blink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hold_caret();
        if self.caret_blinking
            || !self.composer_focus.is_focused(window)
            || !window.is_window_active()
        {
            return;
        }
        self.caret_blinking = true;
        cx.spawn_in(window, async move |this, cx| {
            let mut wait = CARET_SOLID;
            loop {
                cx.background_executor().timer(wait).await;
                let next = this.update_in(cx, |this, window, cx| {
                    if !this.composer_focus.is_focused(window) || !window.is_window_active() {
                        this.caret_blinking = false;
                        this.caret_scale = 1.;
                        cx.notify();
                        return None;
                    }
                    let idle = this.caret_held.elapsed();
                    let scale = caret_scale(idle);
                    if (scale - this.caret_scale).abs() > 0.001 {
                        this.caret_scale = scale;
                        cx.notify();
                    }
                    // Input moves `caret_held` without waking this loop, so the
                    // waits stay short enough to notice it.
                    Some(if idle < CARET_SOLID {
                        (CARET_SOLID - idle).max(CARET_FRAME)
                    } else if idle < CARET_SOLID + CARET_PERIOD * CARET_BLINKS {
                        CARET_FRAME
                    } else {
                        Duration::from_millis(100)
                    })
                });
                match next {
                    Ok(Some(next)) => wait = next,
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// Up in an empty composer brings back the last prompt sent, and more Ups
    /// older ones; Down walks back toward an empty composer. Only while the
    /// recalled text is untouched and the caret is on its first line (Up) or
    /// last line (Down).
    fn recall(&mut self, up: bool) -> bool {
        let draft = &self.env().draft;
        let showing = self
            .history_at
            .filter(|&i| self.prompt_history.get(i) == Some(&**draft));
        let (before, after) = draft.split_at(draft.head());
        let edge = if up {
            !before.contains('\n')
        } else {
            !after.contains('\n')
        };
        let next = match showing {
            None if up && draft.is_empty() => match self.prompt_history.older(None) {
                Some(i) => Some(i),
                None => return false,
            },
            Some(i) if edge && up => Some(self.prompt_history.older(Some(i)).unwrap_or(i)),
            Some(i) if edge => self.prompt_history.newer(i),
            _ => return false,
        };
        self.history_at = next;
        let text = next
            .and_then(|i| self.prompt_history.get(i))
            .unwrap_or("")
            .to_string();
        self.env_mut().draft.set(text);
        true
    }

    /// Up/Down in the composer: the line above or below at the same column,
    /// or the start or end of the draft past its first or last line.
    fn composer_vertical(&mut self, up: bool, extend: bool) {
        let layout = self.composer_layout.borrow().clone();
        let head = self.env().draft.head();
        let target = match layout
            .as_ref()
            .and_then(|l| Some((l, l.position_for_index(head)?)))
        {
            Some((layout, at)) => {
                let x = *self.composer_goal_x.get_or_insert(at.x);
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
        let layout = self.composer_layout.borrow().clone()?;
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
        self.composer_goal_x = None;
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
        self.composer_selecting = true;
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
        if std::mem::take(&mut self.composer_selecting) {
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
        if let Some((path, at, files)) = &self.file_index
            && *path == env_path
            && at.elapsed() < FILE_INDEX_TTL
        {
            return Some(files.clone());
        }
        if !self.file_index_loading {
            self.file_index_loading = true;
            cx.spawn(async move |this, cx| {
                let p = env_path.clone();
                let files = cx
                    .background_spawn(async move { git::ls_files(&p).unwrap_or_default() })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.file_index_loading = false;
                    this.file_index = Some((env_path.clone(), Instant::now(), Arc::new(files)));
                    // The popup may have been waiting for the index.
                    this.update_completion(cx);
                });
            })
            .detach();
        }
        None
    }

    pub(super) fn update_completion(&mut self, cx: &mut Context<Self>) {
        let was_open = self.completion.is_some();
        self.completion = self.compute_completion(cx);
        if was_open || self.completion.is_some() {
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
            let thread = self.active_thread(cx)?;
            let thread = thread.read(cx);
            let name = name.to_lowercase();
            let own = [crate::agent::Choice {
                id: "stage".into(),
                name: "stage".into(),
                description: Some("Stage only the files this session changed".into()),
            }];
            let items: Vec<CompletionItem> = own
                .iter()
                .filter(|o| !thread.commands().iter().any(|c| c.name == o.name))
                .chain(thread.commands())
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
        let Some(comp) = self.completion.take() else {
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
        self.completion = None;
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
            self.composer_flash = Some(
                crate::platform::shortcut_label("Write a prompt first; Ctrl+S saves it for !")
                    .into(),
            );
        } else if let Some(p) = self.saved_prompts.iter().find(|p| p.text == text) {
            self.composer_flash = Some(format!("Already saved as !{}", p.name).into());
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
            self.composer_flash = Some(format!("Saved as !{name}. Type ! to use it.").into());
        }
        cx.notify();
    }

    pub(super) fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env_path = self.env().path.clone();
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
            text.push_str(&crate::agent::attach::wrap_paste(p));
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
        self.history_at = None;
        if self.prompt_history.push(&typed) {
            let bytes = self.prompt_history.bytes();
            cx.background_spawn(async move { crate::history::write(&bytes) })
                .detach();
        }
        self.send_prompt(text, window, cx);
    }

    /// The tab the composer writes to: the active tab, or the session showing beside it
    /// while the other pane (a page, a file) has the focus.
    pub(super) fn composer_tab(&self) -> usize {
        let env = self.env();
        let session = |i: usize| {
            matches!(
                env.tabs.get(i).map(|t| &t.kind),
                Some(TabKind::Session { .. })
            )
        };
        let active = env.active_tab;
        let fresh = matches!(
            env.tabs.get(active).map(|t| &t.kind),
            Some(TabKind::NewSession)
        );
        match env.split_other() {
            Some(other) if !session(active) && !fresh && session(other) => other,
            _ => active,
        }
    }

    /// The chat the composer writes to; its raccoon sits on the composer.
    pub(super) fn composer_chat(&self) -> Option<Entity<crate::agent::ThreadView>> {
        let env = self.env();
        let Some(TabKind::Session { id, .. }) = env.tabs.get(self.composer_tab()).map(|t| &t.kind)
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
        } else if let Some(split) = env.split.filter(|_| window.focused(cx).is_some()) {
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
        if tab != self.env().active_tab {
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
        let session_id = match env.tabs.get(tab).map(|t| &t.kind) {
            Some(TabKind::Session { id, .. }) => Some(id.clone()),
            _ => None,
        };
        let agent = match env.tabs.get(tab).map(|t| &t.kind) {
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
        // A session another process runs can't be handed over from here.
        let can_switch = !session_id
            .as_ref()
            .is_some_and(|id| crate::headless::shown(id));
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
        let focused = self.composer_focus.is_focused(window);
        let entity = cx.entity();
        let focus = self.composer_focus.clone();
        let draft = env.draft.bare();
        let pastes: Vec<(usize, usize)> = self
            .pastes
            .get(&env.path)
            .map(|v| {
                v.iter()
                    .map(|p| (p.lines().count(), p.chars().count()))
                    .collect()
            })
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
        let comp_popup: Option<AnyElement> = self.completion.as_ref().map(|comp| {
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
                            if let Some(c) = &mut this.completion {
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
        let mut show_caret = false;
        let caret_height = CARET_HEIGHT * self.caret_scale;
        if draft.is_empty() {
            self.composer_layout.borrow_mut().take();
        }
        let (layout_cell, reveal, scroll) = (
            self.composer_layout.clone(),
            self.composer_reveal.clone(),
            self.composer_scroll.clone(),
        );
        let text: AnyElement = if draft.is_empty() {
            div()
                .relative()
                .text_color(theme::text_faint())
                .child("Ask, build, / for commands, @ for references...")
                .when(focused && caret_height > 0.2, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left(px(-1.))
                            .top(px((22. - caret_height) / 2.))
                            .w(px(CARET_WIDTH))
                            .h(px(caret_height))
                            .bg(theme::accent()),
                    )
                })
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
            show_caret = focused && sel.is_empty() && caret_height > 0.2;
            styled.into_any_element()
        };

        div()
            .id("composer")
            .track_focus(&self.composer_focus)
            .on_key_down(cx.listener(Self::composer_key))
            .drag_over::<drag::DraggedSession>(|s, _, _, _| {
                s.border_color(theme::accent())
                    .bg(theme::accent().opacity(0.06))
            })
            .on_drop(cx.listener(|this, d: &drag::DraggedSession, window, cx| {
                this.drop_session_on_composer(d, window, cx)
            }))
            // Files dropped from a file manager: images as pictures, the rest as mentions.
            .drag_over::<gpui::ExternalPaths>(|s, _, _, _| {
                s.border_color(theme::accent())
                    .bg(theme::accent().opacity(0.06))
            })
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.push_mentions(paths.paths());
                    window.focus(&this.composer_focus, cx);
                    this.composer_changed(cx);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.composer_focus, cx);
                    cx.notify();
                }),
            )
            // Linux middle-click pastes the primary selection.
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.composer_focus, cx);
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
                let other = if agent == Agent::Droid {
                    Agent::Claude
                } else {
                    Agent::Droid
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
            .when_some(self.composer_flash.clone(), |d, f| {
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
                    .track_scroll(&self.composer_scroll)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                            window.focus(&this.composer_focus, cx);
                            this.composer_click(
                                ev.position,
                                ev.click_count,
                                ev.modifiers.shift,
                                cx,
                            );
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                        if this.composer_selecting && ev.pressed_button == Some(MouseButton::Left) {
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
                                    let Some((layout, head)) = &painted else {
                                        return;
                                    };
                                    *layout_cell.borrow_mut() = Some(layout.clone());
                                    let Some(at) = layout.position_for_index(*head) else {
                                        return;
                                    };
                                    let line = layout.line_height();
                                    if show_caret {
                                        let top = at.y + (line - px(caret_height)) / 2.;
                                        window.paint_quad(fill(
                                            Bounds::new(
                                                point(at.x, top),
                                                size(px(CARET_WIDTH), px(caret_height)),
                                            ),
                                            theme::accent(),
                                        ));
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
                                chip(
                                    "composer-agent",
                                    Some(agent_icon(agent, 14.)),
                                    agent.label().into(),
                                    None,
                                    can_switch,
                                )
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .when(can_switch, |d| {
                                    d.on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                                        this.open_composer_menu(
                                            MenuTarget::AgentMenu,
                                            ev.position(),
                                            cx,
                                        )
                                    }))
                                }),
                            )
                            .when_some(model_label, |d, label| {
                                d.child(
                                    chip(
                                        "composer-model",
                                        fast.then(|| {
                                            icon("flash", 12.)
                                                .text_color(theme::accent())
                                                .into_any_element()
                                        }),
                                        label,
                                        effort_label,
                                        true,
                                    )
                                    .tooltip(ui::tip("Model and reasoning"))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
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
        self.composer_goal_x = None;
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
        let layout = self.composer_layout.borrow().clone();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_expands_like_vs_code() {
        let at = |ms: u64| caret_scale(Duration::from_millis(ms));
        assert_eq!(at(0), 1.);
        assert_eq!(at(499), 1.);
        // Each half holds its end for a fifth: whole, then gone at the middle.
        assert_eq!(at(550), 1.);
        assert!(at(700) < 1. && at(700) > 0.);
        assert_eq!(at(1000), 0.);
        assert_eq!(at(1050), 0.);
        assert!((at(750) - at(1250)).abs() < 1e-4, "grows back as it shrank");
        assert_eq!(at(1500), 1.);
        // Still after the last blink.
        assert_eq!(at(10_500), 1.);
        assert_eq!(at(60_000), 1.);
    }
}
