//! Diffs in the transcript: the cards edit tools get, and a turn's changes behind its "N files" chip.

use super::*;

impl ThreadView {
    pub(super) fn toggle_changes(&mut self, turn_start: usize, cx: &mut Context<Self>) {
        let open = !self.changes_open.remove(&turn_start);
        if open {
            self.changes_open.insert(turn_start);
        }
        self.opened(Fold::Changes, turn_start, open);
        self.remeasure_entry(turn_start, cx);
        cx.notify();
    }

    /// Every diff a settled turn made, file by file; a click opens the file.
    pub(super) fn turn_changes(
        &self,
        edits: &TurnEdits,
        turn_start: usize,
        base: usize,
        cwd: &Path,
        cx: &Context<Self>,
    ) -> AnyElement {
        let mut col = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(6.))
            .min_w_0()
            .px(crate::ui::sp(16.))
            .pt(crate::ui::sp(2.))
            .pb(crate::ui::sp(8.));
        for (n, (path, diffs)) in edits.files.iter().enumerate() {
            let full = if path.is_relative() {
                cwd.join(path)
            } else {
                path.to_path_buf()
            };
            let exists = full.exists();
            for (k, diff) in diffs.iter().enumerate() {
                let full = full.clone();
                let at = diff.at;
                col = col.child(
                    div()
                        .id(("turn-diff", n * 1000 + k))
                        .min_w_0()
                        .when(exists, |d| {
                            d.cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let entries = this.thread.read(cx).entries();
                                    let (old, new) =
                                        match (at, entries.get(turn_start).map(Arc::as_ref)) {
                                            (DiffAt::Tool(i, ci), _) => {
                                                match entries.get(base + i).map(Arc::as_ref) {
                                                    Some(Entry::Tool(t)) => match t.content.get(ci)
                                                    {
                                                        Some(ToolContent::Diff {
                                                            old_text,
                                                            new_text,
                                                            ..
                                                        }) => (
                                                            old_text.as_deref(),
                                                            Some(new_text.as_str()),
                                                        ),
                                                        _ => (None, None),
                                                    },
                                                    _ => (None, None),
                                                }
                                            }
                                            (
                                                DiffAt::Shell(i),
                                                Some(Entry::User { shell_changes, .. }),
                                            ) => shell_changes.get(i).map_or((None, None), |c| {
                                                (c.old.as_deref(), Some(c.new.as_str()))
                                            }),
                                            _ => (None, None),
                                        };
                                    let line = new.and_then(|new| {
                                        std::fs::read_to_string(&full)
                                            .ok()
                                            .and_then(|f| changed_line(&f, old, new))
                                    });
                                    cx.emit(ThreadViewEvent::OpenFile(full.clone(), line))
                                }))
                        })
                        .child(diff_card(
                            path,
                            diff.old,
                            diff.new,
                            cwd,
                            &Self::reveal_handle(cx),
                        )),
                );
            }
        }
        col.into_any_element()
    }
}

/// What a turn's finished edits changed: each file in first-touched order with
/// its diffs, and the lines added and removed in all.
pub(super) struct TurnEdits<'a> {
    pub(super) files: Vec<(&'a Path, Vec<TurnDiff<'a>>)>,
    pub(super) added: u32,
    pub(super) removed: u32,
}

pub(super) struct TurnDiff<'a> {
    pub(super) at: DiffAt,
    pub(super) old: Option<&'a str>,
    pub(super) new: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum DiffAt {
    /// The tool's index in the turn's content, and the diff's in the tool's.
    Tool(usize, usize),
    /// The index in the turn's shell changes.
    Shell(usize),
}

/// The diffs of the edit tools, then what the shell commands changed besides.
pub(super) fn turn_edits<'a>(shell: &'a [ShellChange], content: &'a [Arc<Entry>]) -> TurnEdits<'a> {
    let mut edits = TurnEdits {
        files: Vec::new(),
        added: 0,
        removed: 0,
    };
    for (i, e) in content.iter().enumerate() {
        let Entry::Tool(t) = &**e else { continue };
        if t.status != ToolStatus::Completed {
            continue;
        }
        let mut any = false;
        for (k, c) in t.content.iter().enumerate() {
            let ToolContent::Diff {
                path,
                old_text,
                new_text,
            } = c
            else {
                continue;
            };
            any = true;
            let diff = TurnDiff {
                at: DiffAt::Tool(i, k),
                old: old_text.as_deref(),
                new: new_text,
            };
            match edits.files.iter_mut().find(|(p, _)| *p == path.as_path()) {
                Some((_, diffs)) => diffs.push(diff),
                None => edits.files.push((path, vec![diff])),
            }
        }
        if any {
            edits.added += t.added;
            edits.removed += t.removed;
        }
    }
    for (k, c) in shell.iter().enumerate() {
        edits.files.push((
            &c.path,
            vec![TurnDiff {
                at: DiffAt::Shell(k),
                old: c.old.as_deref(),
                new: &c.new,
            }],
        ));
        edits.added += c.added;
        edits.removed += c.removed;
    }
    edits
}

/// The line of `file` a diff's first change is on, while its new text is
/// still there word for word.
pub(super) fn changed_line(file: &str, old: Option<&str>, new: &str) -> Option<usize> {
    let first = diff::line_diff(old.unwrap_or(""), new)
        .into_iter()
        .find(|l| l.kind != LineKind::Context)?;
    let at = file.find(new)?;
    Some(file[..at].matches('\n').count() + first.number.unwrap_or(1).max(1) as usize)
}

pub(super) fn diff_counts(added: u32, removed: u32) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .gap(crate::ui::sp(4.))
        .text_size(px(crate::ui::TEXT_XS))
        .font_weight(FontWeight::SEMIBOLD)
        .when(added > 0, |d| {
            d.child(
                div()
                    .text_color(theme::diff_add())
                    .child(format!("+{added}")),
            )
        })
        .when(removed > 0, |d| {
            d.child(
                div()
                    .text_color(theme::diff_del())
                    .child(format!("-{removed}")),
            )
        })
}

/// What a diff card shows, worked out once per diff: cards are drawn every
/// frame while they are visible, and the line diff is quadratic in places.
pub(super) struct DiffCard {
    added: u32,
    removed: u32,
    /// Lines in the hunks, of which `rows` holds the first `PREVIEW_LINES`.
    total: usize,
    /// `None` is the gap between two hunks.
    rows: Vec<Option<(LineKind, Option<u32>)>>,
    texts: Vec<String>,
    spans: Option<Rc<Vec<LineSpans>>>,
}

thread_local! {
    /// The diff cards on screen, by their content.
    static DIFF_CARDS: RefCell<HashMap<u64, Rc<DiffCard>>> = RefCell::new(HashMap::new());
}

pub(super) fn diff_card_model(path: &Path, old: Option<&str>, new: &str) -> Rc<DiffCard> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (path, old, new).hash(&mut h);
    let key = h.finish();
    if let Some(card) = DIFF_CARDS.with(|cache| cache.borrow().get(&key).cloned()) {
        return card;
    }
    let card = Rc::new(build_card(path, old, new));
    DIFF_CARDS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() > 256 {
            cache.clear();
        }
        cache.insert(key, card.clone());
    });
    card
}

fn build_card(path: &Path, old: Option<&str>, new: &str) -> DiffCard {
    let lines = diff::line_diff(old.unwrap_or(""), new);
    let (added, removed) = diff::stats(&lines);
    let hunks = diff::hunks(&lines, 3);
    let shown = &hunks[..hunks.len().min(PREVIEW_LINES)];
    let texts: Vec<String> = shown
        .iter()
        .map(|l| l.map_or(String::new(), |l| l.text.replace('\t', "    ")))
        .collect();
    let spans = crate::highlight::highlight(&path.to_string_lossy(), &texts).map(Rc::new);
    DiffCard {
        added,
        removed,
        total: hunks.len(),
        rows: shown
            .iter()
            .map(|l| l.map(|l| (l.kind, l.number)))
            .collect(),
        texts,
        spans,
    }
}

/// One line of code in its syntax colors over `base`; `dim` fades the colors.
/// With it, the secrets it hides, which a click shows.
pub(super) fn colored_line(
    text: &str,
    spans: Option<&LineSpans>,
    base: Hsla,
    dim: f32,
) -> (StyledText, Vec<String>) {
    let f = font(theme::FONT_MONO);
    let run = |len: usize, color: Hsla| TextRun {
        len,
        font: f.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut runs = Vec::new();
    let mut at = 0;
    for (range, color) in spans.into_iter().flatten() {
        let (a, b) = (range.start.min(text.len()), range.end.min(text.len()));
        if a < at || a >= b {
            continue;
        }
        if a > at {
            runs.push(run(a - at, base));
        }
        runs.push(run(b - a, color.opacity(dim)));
        at = b;
    }
    if text.len() > at {
        runs.push(run(text.len() - at, base));
    }
    // Masked to the same length, so the syntax colors still line up.
    let Some(m) = crate::redact::in_chat(text) else {
        return (
            StyledText::new(SharedString::from(text.to_string())).with_runs(runs),
            Vec::new(),
        );
    };
    let runs = if crate::redact::on() {
        runs
    } else {
        let ranges: Vec<_> = m.hidden.iter().map(|h| h.range.clone()).collect();
        crate::agent::markdown::secret_runs(runs, &ranges)
    };
    let hidden = m.hidden.into_iter().map(|h| h.value).collect();
    (
        StyledText::new(SharedString::from(m.text)).with_runs(runs),
        hidden,
    )
}

pub(super) fn diff_bg(kind: LineKind) -> Option<Hsla> {
    match kind {
        LineKind::Add => Some(theme::diff_add_bg()),
        LineKind::Del => Some(theme::diff_del_bg()),
        LineKind::Context => None,
    }
}

/// A file edit as a card: file header with +/- counts, then the changed
/// lines with a little context.
pub(super) fn diff_card(
    path: &Path,
    old: Option<&str>,
    new: &str,
    cwd: &Path,
    reveal: &Reveal,
) -> AnyElement {
    let card = diff_card_model(path, old, new);
    let DiffCard {
        added,
        removed,
        total,
        ..
    } = *card;
    let name = rel_path(path, cwd);
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
        .bg(theme::sunk())
        .overflow_hidden()
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .px(crate::ui::sp(10.))
                .py(crate::ui::sp(8.))
                .child(icon("file01", 14.).text_color(theme::text_subtle()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(theme::FONT_MONO)
                        .text_size(px(crate::ui::TEXT_SM))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text_muted())
                        .child(name),
                )
                .child(
                    div()
                        .flex()
                        .gap(crate::ui::sp(4.))
                        .text_size(px(crate::ui::TEXT_XS))
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(added > 0, |d| {
                            d.child(
                                div()
                                    .text_color(theme::diff_add())
                                    .child(format!("+{added}")),
                            )
                        })
                        .when(removed > 0, |d| {
                            d.child(
                                div()
                                    .text_color(theme::diff_del())
                                    .child(format!("-{removed}")),
                            )
                        }),
                ),
        )
        .when(total > 0, |d| {
            d.child(div().h(px(1.)).bg(theme::fill_selected()))
                .child(
                    div()
                        .id(SharedString::from(format!("diff-lines-{}", path.display())))
                        .flex()
                        .overflow_x_scroll()
                        .restrict_scroll_to_axis()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_none()
                                .min_w_full()
                                .py(crate::ui::sp(2.))
                                .children(card.rows.iter().enumerate().map(|(n, row)| {
                                    match *row {
                                        None => div()
                                            .h(px(18.))
                                            .pl(crate::ui::sp(28.))
                                            .font_family(theme::FONT_MONO)
                                            .text_size(px(crate::ui::TEXT_XS))
                                            .text_color(theme::text_faint())
                                            .child("⋯"),
                                        Some((kind, number)) => {
                                            let (mark, mark_color, bar) = match kind {
                                                LineKind::Add => (
                                                    "+",
                                                    Hsla::from(theme::diff_add()),
                                                    Some(theme::diff_add_bar()),
                                                ),
                                                LineKind::Del => (
                                                    "−",
                                                    Hsla::from(theme::diff_del()),
                                                    Some(theme::diff_del_bar()),
                                                ),
                                                LineKind::Context => {
                                                    (" ", gpui::transparent_black(), None)
                                                }
                                            };
                                            div()
                                                .relative()
                                                .flex()
                                                .items_center()
                                                .h(px(18.))
                                                .when_some(diff_bg(kind), |d, bg| d.bg(bg))
                                                .when_some(bar, |d, bar| {
                                                    d.child(
                                                        div()
                                                            .absolute()
                                                            .left_0()
                                                            .top_0()
                                                            .bottom_0()
                                                            .w(px(2.))
                                                            .bg(bar),
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .flex_none()
                                                        .w(px(28.))
                                                        .pr(crate::ui::sp(4.))
                                                        .flex()
                                                        .justify_end()
                                                        .font_family(theme::FONT_MONO)
                                                        .text_size(px(crate::ui::TEXT_XS))
                                                        .text_color(theme::text_faint())
                                                        .child(number.map_or(String::new(), |n| {
                                                            n.to_string()
                                                        })),
                                                )
                                                .child(
                                                    div()
                                                        .flex_none()
                                                        .w(px(12.))
                                                        .flex()
                                                        .justify_center()
                                                        .font_family(theme::FONT_MONO)
                                                        .text_size(px(crate::ui::TEXT_XS))
                                                        .font_weight(FontWeight::MEDIUM)
                                                        .text_color(mark_color)
                                                        .child(mark),
                                                )
                                                .child({
                                                    let base = if kind == LineKind::Context {
                                                        theme::mix(0.55)
                                                    } else {
                                                        theme::mix(0.85)
                                                    };
                                                    let dim = if kind == LineKind::Context {
                                                        0.7
                                                    } else {
                                                        1.
                                                    };
                                                    let (line, hidden) = colored_line(
                                                        &card.texts[n],
                                                        card.spans.as_ref().and_then(|s| s.get(n)),
                                                        base,
                                                        dim,
                                                    );
                                                    reveal
                                                        .on(
                                                            div()
                                                                .id(("diff-line", n))
                                                                .flex_none()
                                                                .pr(crate::ui::sp(8.))
                                                                .whitespace_nowrap()
                                                                .font_family(theme::FONT_MONO)
                                                                .text_size(px(crate::ui::TEXT_XS)),
                                                            hidden,
                                                        )
                                                        .child(line)
                                                })
                                        }
                                    }
                                })),
                        ),
                )
                .when(total > PREVIEW_LINES, |d| {
                    d.child(
                        div()
                            .px(crate::ui::sp(10.))
                            .py(crate::ui::sp(4.))
                            .text_size(px(crate::ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(format!("… {} more lines", total - PREVIEW_LINES)),
                    )
                })
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diff_card_is_worked_out_once_per_diff() {
        let path = Path::new("src/lib.rs");
        let old = "fn a() {}\nfn b() {}\n";
        let new = "fn a() {}\nfn c() {}\n";
        let first = diff_card_model(path, Some(old), new);
        let again = diff_card_model(path, Some(old), new);
        assert!(Rc::ptr_eq(&first, &again));
        assert_eq!((first.added, first.removed), (1, 1));
        assert_eq!(first.rows.len(), first.texts.len());
        let other = diff_card_model(path, Some(old), "fn a() {}\n");
        assert!(!Rc::ptr_eq(&first, &other));
    }
}
