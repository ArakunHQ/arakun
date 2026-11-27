//! Ctrl+F in the conversation: finding matches, revealing folded ones and the bar itself.

use super::*;

impl ThreadView {
    /// Opens the search bar, or focuses it again. A one-line selection is
    /// what gets searched.
    pub fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let seed = self
            .selected_text(false)
            .filter(|t| !t.trim().is_empty() && !t.contains('\n') && t.len() <= 200);
        let input = match &self.search {
            Some(s) => s.input.clone(),
            None => {
                let input = cx.new(|cx| TextInput::new("Find in session", cx));
                let sub = cx.subscribe_in(
                    &input,
                    window,
                    |this, _, ev: &InputEvent, window, cx| match ev {
                        InputEvent::Changed => this.search_changed(window, cx),
                        InputEvent::Submit | InputEvent::Down => this.step_search(1, window, cx),
                        InputEvent::Up => this.step_search(-1, window, cx),
                        InputEvent::Cancel => this.close_search(window, cx),
                        _ => {}
                    },
                );
                self.search = Some(Search {
                    input: input.clone(),
                    query: String::new(),
                    hits: Vec::new(),
                    current: None,
                    stale: false,
                    restore: window.focused(cx),
                    revealed: None,
                    _sub: sub,
                });
                input
            }
        };
        input.update(cx, |i, cx| {
            if let Some(seed) = seed {
                i.set_text(seed, cx);
            }
            i.select_all_text(cx);
        });
        window.focus(&input.focus_handle(cx), cx);
        cx.notify();
    }

    pub(super) fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.search.take() else {
            return;
        };
        window.focus(s.restore.as_ref().unwrap_or(&self.focus), cx);
        cx.notify();
    }

    /// Every match of `query`, counted the way the surfaces paint them.
    pub(super) fn collect_hits(&mut self, query: &str, cx: &App) -> Vec<(usize, usize)> {
        let mut hits = Vec::new();
        if query.is_empty() {
            return hits;
        }
        let thread = self.thread.clone();
        for (ix, e) in thread.read(cx).entries().iter().enumerate() {
            let n = match e {
                Entry::User { text, .. } => {
                    search::find_all(&crate::agent::attach::split_pastes(text).0, query).len()
                }
                Entry::Agent { text } | Entry::Thought { text } => {
                    let doc = self.markdown_for(ix, text);
                    let mut pieces = Vec::new();
                    markdown::pieces(&doc.blocks, &mut pieces);
                    pieces
                        .iter()
                        .map(|p| search::find_all(p, query).len())
                        .sum()
                }
                _ => 0,
            };
            hits.extend((0..n).map(|k| (ix, k)));
        }
        hits
    }

    pub(super) fn search_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = &self.search else {
            return;
        };
        let query = s.input.read(cx).text().to_string();
        if query == s.query {
            return;
        }
        let hits = self.collect_hits(&query, cx);
        // The first match from the top of what is on screen, like a browser.
        let from = self
            .row_start_entry(self.list.logical_scroll_top().item_ix)
            .unwrap_or(usize::MAX);
        let current = hits
            .iter()
            .position(|h| h.0 >= from)
            .or(hits.len().checked_sub(1));
        if let Some(s) = &mut self.search {
            s.query = query;
            s.hits = hits;
            s.current = current;
            s.stale = false;
        }
        self.show_hit(window, cx);
        cx.notify();
    }

    /// Recounts after the transcript changed, staying on the same match.
    pub(super) fn refresh_search(&mut self, cx: &App) {
        let Some(s) = self.search.as_ref().filter(|s| s.stale) else {
            return;
        };
        let query = s.query.clone();
        let was = s.current.and_then(|c| s.hits.get(c).copied());
        let hits = self.collect_hits(&query, cx);
        let current = match was {
            Some(h) => hits
                .iter()
                .position(|x| *x == h)
                .or(hits.len().checked_sub(1)),
            None => None,
        };
        if let Some(s) = &mut self.search {
            s.hits = hits;
            s.current = current;
            s.stale = false;
        }
    }

    pub(super) fn step_search(
        &mut self,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(s) = &mut self.search else {
            return;
        };
        if s.hits.is_empty() {
            return;
        }
        let n = s.hits.len() as isize;
        s.current = Some(match s.current {
            Some(c) => (c as isize + delta).rem_euclid(n) as usize,
            None if delta < 0 => s.hits.len() - 1,
            None => 0,
        });
        self.show_hit(window, cx);
        cx.notify();
    }

    /// Unfolds what hides the current match and scrolls it into view.
    pub(super) fn show_hit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(old) = self.search.as_mut().and_then(|s| s.revealed.take()) {
            self.unreveal(old, cx);
        }
        let Some(entry) = self
            .search
            .as_ref()
            .and_then(|s| s.hits.get(s.current?))
            .map(|h| h.0)
        else {
            return;
        };
        let Some(turn) = self.turns.iter().position(|t| t.contains(&entry)) else {
            return;
        };
        let revealed = self.reveal_entry(turn, entry, cx);
        if let Some(s) = &mut self.search {
            s.revealed = Some(revealed);
        }
        self.spot.set(None);
        self.settle_tries = 0;
        self.list.pause_following_tail();
        if let Some(row_ix) = self.row_for_entry(entry, cx) {
            self.list.scroll_to_reveal_item(row_ix);
        }
        cx.on_next_frame(window, |this, window, cx| {
            this.settle_search_scroll(window, cx)
        });
    }

    /// Brings the current match to a third of the way down once it has been
    /// laid out; a turn can be taller than the screen.
    pub(super) fn settle_search_scroll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spot) = self.spot.get() else {
            self.settle_tries += 1;
            if self.settle_tries < 4 {
                cx.on_next_frame(window, |this, window, cx| {
                    this.settle_search_scroll(window, cx)
                });
            }
            return;
        };
        let view = self.list.viewport_bounds();
        // The bar covers the top of the list.
        if spot.top() < view.top() + px(96.) || spot.bottom() > view.bottom() - px(48.) {
            self.list
                .scroll_by(spot.top() - (view.top() + view.size.height * 0.35));
            cx.notify();
        }
    }

    pub(super) fn unreveal(&mut self, r: Revealed, cx: &App) {
        if let Some(f) = r.fold {
            self.folds.remove(&f);
        }
        match r.phase {
            Some((key, Some(was))) => {
                self.phase_open.insert(key, was);
            }
            Some((key, None)) => {
                self.phase_open.remove(&key);
            }
            None => {}
        }
        if let Some(row) = r.row {
            self.open.remove(&row);
        }
        self.sync_rows(None, cx);
        if r.turn < self.turns.len() {
            self.remeasure_turn_rows(r.turn);
        }
    }

    /// Opens the fold, phase, row, or long prompt `entry` is hidden in.
    pub(super) fn reveal_entry(&mut self, turn: usize, entry: usize, cx: &App) -> Revealed {
        let range = self.turns[turn].clone();
        let t = self.thread.read(cx);
        let entries = &t.entries()[range.clone()];
        let k = entry - range.start;
        let mut fold = false;
        let mut phase = None;
        let mut row = false;
        match &entries[k] {
            Entry::User { text, .. } => {
                let (typed, _) = crate::agent::attach::split_pastes(text);
                row = typed.lines().count() > 8 || typed.len() > 700;
            }
            _ => {
                let live = self.is_live(turn, t);
                let parts = turn_parts(entries, live);
                let content = &entries[parts.body_start..entries.len() - parts.trailing_notices];
                let base = range.start + parts.body_start;
                if let Some(i) = k.checked_sub(parts.body_start)
                    && i < content.len()
                    && !parts.reply.contains(&i)
                {
                    fold = !live;
                    for p in phases_of(content, &parts.work) {
                        if p.head == Some(i) {
                            break;
                        }
                        if p.steps.contains(&i) {
                            if p.head.is_some() || p.steps.len() > 1 {
                                phase = Some(base + p.head.unwrap_or(p.steps[0]));
                            }
                            row = true;
                            break;
                        }
                    }
                }
            }
        }
        let mut revealed = Revealed {
            turn,
            fold: None,
            phase: None,
            row: None,
        };
        if fold && self.folds.insert(range.start) {
            revealed.fold = Some(range.start);
        }
        if let Some(key) = phase {
            let was = self.phase_open.insert(key, true);
            if was != Some(true) {
                revealed.phase = Some((key, was));
            }
        }
        if row && self.open.insert(entry) {
            revealed.row = Some(entry);
        }
        self.sync_rows(Some(entry), cx);
        revealed
    }

    /// The search matches surface `ix` paints.
    pub(super) fn find_for(&self, ix: usize) -> Option<Find<'_>> {
        let s = self.search.as_ref().filter(|s| !s.query.is_empty())?;
        Some(Find {
            query: &s.query,
            current: s
                .current
                .and_then(|c| s.hits.get(c))
                .filter(|h| h.0 == ix)
                .map(|h| h.1),
            spot: &self.spot,
        })
    }

    pub(super) fn render_search_bar(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let s = self.search.as_ref()?;
        let count = match s.current {
            _ if s.query.is_empty() => String::new(),
            Some(c) => format!("{}/{}", c + 1, s.hits.len()),
            None => "No results".to_string(),
        };
        let button = |id: &'static str, name: &'static str| {
            div()
                .id(id)
                .size(px(24.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(crate::ui::rad(8.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::fill_selected()))
                .child(icon(name, 14.).text_color(theme::text_subtle()))
        };
        let found = !s.hits.is_empty();
        Some(
            div()
                .id("search-bar")
                .absolute()
                .top(px(8.))
                .right(px(16.))
                .w(px(340.))
                .h(px(36.))
                .flex()
                .items_center()
                .gap(crate::ui::sp(4.))
                .pl(crate::ui::sp(10.))
                .pr(crate::ui::sp(4.))
                .rounded(crate::ui::rad(12.))
                .border_1()
                .border_color(theme::border_strong())
                .bg(theme::dialog())
                .shadow_lg()
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                    let ks = &ev.keystroke;
                    if ks.key == "enter" && ks.modifiers.shift {
                        this.step_search(-1, window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(icon("search01", 14.).text_color(theme::text_subtle()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(crate::ui::TEXT_MD))
                        .child(s.input.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .px(crate::ui::sp(4.))
                        .text_size(px(crate::ui::TEXT_SM))
                        .text_color(if found || s.query.is_empty() {
                            theme::mix(0.5)
                        } else {
                            red().opacity(0.8)
                        })
                        .child(count),
                )
                .child(
                    button("search-prev", "arrow-up01")
                        .when(!found, |d| d.opacity(0.4))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.step_search(-1, window, cx)),
                        ),
                )
                .child(
                    button("search-next", "arrow-down01")
                        .when(!found, |d| d.opacity(0.4))
                        .on_click(
                            cx.listener(|this, _, window, cx| this.step_search(1, window, cx)),
                        ),
                )
                .child(
                    button("search-close", "cancel01")
                        .on_click(cx.listener(|this, _, window, cx| this.close_search(window, cx))),
                )
                .map(|d| {
                    crate::ui::motion::enter(
                        d,
                        "search-bar-in",
                        crate::ui::motion::From::Above,
                        4.,
                        crate::ui::motion::FAST,
                    )
                })
                .into_any_element(),
        )
    }
}
