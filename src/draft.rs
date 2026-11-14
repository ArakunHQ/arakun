//! The session composer's draft: text with a caret and a selection, edited the
//! way a text field is. Offsets are byte indices, kept on char boundaries.

use std::ops::{Deref, Range};

use unicode_segmentation::UnicodeSegmentation as _;

#[derive(Clone, Debug, Default)]
pub struct Draft {
    text: String,
    /// Where the selection started; equal to `head` when nothing is selected.
    anchor: usize,
    /// The caret.
    head: usize,
    /// Text an input method is still composing.
    marked: Option<Range<usize>>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// The kind of the last edit, while further edits of that kind join its undo step.
    last: Option<Edit>,
}

/// The text and selection as they were before an edit.
#[derive(Clone, Debug)]
struct Step {
    text: String,
    anchor: usize,
    head: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Edit {
    Type,
    Erase,
    Compose,
}

const UNDO_STEPS: usize = 200;

impl Deref for Draft {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl Draft {
    /// A draft read back from disk, with the caret at `head`.
    pub fn restore(text: String, head: usize) -> Self {
        let mut d = Self { text, ..Self::default() };
        d.head = d.floor(head);
        d.anchor = d.head;
        d
    }

    /// A copy without the undo history, for reading the draft cheaply.
    pub fn bare(&self) -> Self {
        Self {
            text: self.text.clone(),
            anchor: self.anchor,
            head: self.head,
            marked: self.marked.clone(),
            ..Self::default()
        }
    }

    pub fn head(&self) -> usize {
        self.head
    }

    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn has_selection(&self) -> bool {
        self.anchor != self.head
    }

    /// Whether the caret is at the start of the selection.
    pub fn reversed(&self) -> bool {
        self.head < self.anchor
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection()]
    }

    /// Replaces the whole text, with the caret at its end.
    pub fn set(&mut self, text: impl Into<String>) {
        let text = text.into();
        if text != self.text {
            self.record(None);
        }
        self.text = text;
        self.head = self.text.len();
        self.anchor = self.head;
        self.marked = None;
    }

    pub fn clear(&mut self) {
        self.set(String::new());
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.head = self.text.len();
        self.last = None;
    }

    /// Selects `range`, with the caret at its end.
    pub fn select(&mut self, range: Range<usize>) {
        self.anchor = self.floor(range.start);
        self.head = self.floor(range.end);
        self.last = None;
    }

    /// Puts the caret at `ix`; `extend` keeps the anchor, selecting what lies between.
    pub fn move_to(&mut self, ix: usize, extend: bool) {
        self.head = self.floor(ix);
        if !extend {
            self.anchor = self.head;
        }
        self.last = None;
    }

    /// Left arrow: one letter, or one word with `word`. Without `extend` a
    /// selection collapses to its start instead.
    pub fn left(&mut self, word: bool, extend: bool) {
        if !extend && self.has_selection() {
            return self.move_to(self.selection().start, false);
        }
        let ix = if word { previous_word(&self.text, self.head) } else { previous_boundary(&self.text, self.head) };
        self.move_to(ix, extend);
    }

    pub fn right(&mut self, word: bool, extend: bool) {
        if !extend && self.has_selection() {
            return self.move_to(self.selection().end, false);
        }
        let ix = if word { next_word(&self.text, self.head) } else { next_boundary(&self.text, self.head) };
        self.move_to(ix, extend);
    }

    /// Home: the start of the caret's line.
    pub fn home(&mut self, extend: bool) {
        self.move_to(self.line_at(self.head).start, extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.move_to(self.line_at(self.head).end, extend);
    }

    /// Replaces `range` with `s` and puts the caret after it.
    pub fn replace(&mut self, range: Range<usize>, s: &str) {
        self.record(None);
        self.splice(range, s);
    }

    /// Typing: `s` replaces the text being composed, else the selection.
    /// Letters typed in a row undo together, a word at a time.
    pub fn insert(&mut self, s: &str) {
        let kind = if self.marked.is_some() {
            Some(Edit::Compose)
        } else if !self.has_selection() && s != "\n" && s.graphemes(true).count() == 1 {
            let after_word = self.text[..self.head].chars().next_back().is_some_and(|c| !c.is_whitespace());
            if after_word && s.chars().all(char::is_whitespace) {
                self.last = None;
            }
            Some(Edit::Type)
        } else {
            None
        };
        self.record(kind);
        let range = self.marked.clone().unwrap_or_else(|| self.selection());
        self.splice(range, s);
    }

    /// Inserts `s` at the caret as a word of its own, after a space unless
    /// one is there already.
    pub fn insert_word(&mut self, s: &str) {
        let range = self.selection();
        let glued = self.text[..range.start].chars().next_back().is_some_and(|c| !c.is_whitespace());
        if glued {
            self.replace(range, &format!(" {s}"));
        } else {
            self.replace(range, s);
        }
    }

    /// Ctrl+Z: back to how the draft was before the last edit.
    pub fn undo(&mut self) -> bool {
        let Some(step) = self.undo.pop() else {
            return false;
        };
        let now = self.step();
        self.redo.push(now);
        self.apply(step);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(step) = self.redo.pop() else {
            return false;
        };
        let now = self.step();
        self.undo.push(now);
        self.apply(step);
        true
    }

    fn step(&self) -> Step {
        Step { text: self.text.clone(), anchor: self.anchor, head: self.head }
    }

    fn apply(&mut self, step: Step) {
        self.text = step.text;
        self.anchor = self.floor(step.anchor);
        self.head = self.floor(step.head);
        self.marked = None;
        self.last = None;
    }

    /// Keeps the draft as it is now for undo, unless this edit continues the
    /// last one (`kind` matching it).
    fn record(&mut self, kind: Option<Edit>) {
        if kind.is_some() && kind == self.last {
            return;
        }
        self.last = kind;
        let now = self.step();
        self.undo.push(now);
        if self.undo.len() > UNDO_STEPS {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn splice(&mut self, range: Range<usize>, s: &str) {
        let (a, b) = (self.floor(range.start), self.floor(range.end));
        let range = a.min(b)..a.max(b);
        self.text.replace_range(range.clone(), s);
        self.head = range.start + s.len();
        self.anchor = self.head;
        self.marked = None;
    }

    /// Input method composition: `s` replaces `range` (else the text being
    /// composed, else the selection) and stays marked; `selected` is a
    /// selection inside `s`, else the caret goes after it.
    pub fn compose(&mut self, range: Option<Range<usize>>, s: &str, selected: Option<Range<usize>>) {
        let range = range.or_else(|| self.marked.clone()).unwrap_or_else(|| self.selection());
        self.record(Some(Edit::Compose));
        self.splice(range, s);
        let start = self.head - s.len();
        self.marked = (!s.is_empty()).then_some(start..self.head);
        if let Some(sel) = selected {
            self.anchor = self.floor(start + sel.start.min(s.len()));
            self.head = self.floor(start + sel.end.min(s.len()));
        }
    }

    pub fn unmark(&mut self) {
        self.marked = None;
    }

    /// Backspace: the selection, else the letter (or word) before the caret.
    pub fn backspace(&mut self, word: bool) {
        if self.has_selection() {
            return self.replace(self.selection(), "");
        }
        let from = if word { previous_word(&self.text, self.head) } else { previous_boundary(&self.text, self.head) };
        self.erase(from..self.head, word);
    }

    /// Delete: the selection, else the letter (or word) after the caret.
    pub fn delete(&mut self, word: bool) {
        if self.has_selection() {
            return self.replace(self.selection(), "");
        }
        let to = if word { next_word(&self.text, self.head) } else { next_boundary(&self.text, self.head) };
        self.erase(self.head..to, word);
    }

    /// Letters erased one by one undo together; whole words one at a time.
    fn erase(&mut self, range: Range<usize>, word: bool) {
        if range.is_empty() {
            return;
        }
        self.record((!word).then_some(Edit::Erase));
        self.splice(range, "");
    }

    /// The word (or run of spaces or punctuation) at `ix`, for double-clicks.
    pub fn word_at(&self, ix: usize) -> Range<usize> {
        let ix = self.floor(ix);
        let mut last = ix..ix;
        for (start, word) in self.text.split_word_bound_indices() {
            let range = start..start + word.len();
            if range.contains(&ix) {
                return range;
            }
            last = range;
        }
        // At the very end: the word just before.
        if ix == self.text.len() { last } else { ix..ix }
    }

    /// The line holding `ix`, without its newline.
    pub fn line_at(&self, ix: usize) -> Range<usize> {
        let ix = self.floor(ix);
        let start = self.text[..ix].rfind('\n').map_or(0, |i| i + 1);
        let end = self.text[ix..].find('\n').map_or(self.text.len(), |i| ix + i);
        start..end
    }

    /// The word being typed before the caret, for completion: where it starts
    /// and its text. `None` while something is selected.
    pub fn token(&self) -> Option<(usize, &str)> {
        if self.has_selection() {
            return None;
        }
        let before = &self.text[..self.head];
        let start = before
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8());
        Some((start, &self.text[start..self.head]))
    }

    pub fn byte_to_utf16(&self, ix: usize) -> usize {
        self.text[..self.floor(ix)].encode_utf16().count()
    }

    pub fn utf16_to_byte(&self, ix: usize) -> usize {
        byte_of_utf16(&self.text, ix)
    }

    /// `ix` clamped to the text and moved back onto a char boundary.
    fn floor(&self, ix: usize) -> usize {
        let mut ix = ix.min(self.text.len());
        while !self.text.is_char_boundary(ix) {
            ix -= 1;
        }
        ix
    }
}

/// The byte offset in `text` of UTF-16 offset `ix`, which input methods use.
pub fn byte_of_utf16(text: &str, ix: usize) -> usize {
    let mut units = 0;
    for (i, c) in text.char_indices() {
        if units >= ix {
            return i;
        }
        units += c.len_utf16();
    }
    text.len()
}

fn previous_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true).rev().find_map(|(i, _)| (i < offset).then_some(i)).unwrap_or(0)
}

fn next_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true).find_map(|(i, _)| (i > offset).then_some(i)).unwrap_or(text.len())
}

fn word_break(c: char) -> bool {
    c.is_whitespace() || c == '/'
}

/// Word motions stop at `/` too, so Ctrl+Backspace in a path removes one segment.
fn previous_word(text: &str, offset: usize) -> usize {
    let trimmed = text[..offset].trim_end_matches(word_break);
    trimmed.char_indices().rev().find(|(_, c)| word_break(*c)).map_or(0, |(i, c)| i + c.len_utf8())
}

fn next_word(text: &str, offset: usize) -> usize {
    let rest = &text[offset..];
    let after = rest.trim_start_matches(word_break);
    let skip = rest.len() - after.len();
    offset + skip + after.find(word_break).unwrap_or(after.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(text: &str) -> Draft {
        let mut d = Draft::default();
        d.set(text);
        d
    }

    #[test]
    fn types_and_deletes_at_the_caret() {
        let mut d = draft("hola mundo");
        d.move_to(4, false);
        d.insert(",");
        assert_eq!(&*d, "hola, mundo");
        assert_eq!(d.head(), 5);
        d.backspace(false);
        d.delete(false);
        assert_eq!(&*d, "holamundo");
        d.backspace(true);
        assert_eq!(&*d, "mundo");
        assert_eq!(d.head(), 0);
        d.delete(true);
        assert_eq!(&*d, "");
    }

    #[test]
    fn backspace_takes_whole_letters() {
        let mut d = draft("ok 👍🏽");
        d.backspace(false);
        assert_eq!(&*d, "ok ");
        let mut d = draft("cafe\u{301}");
        d.backspace(false);
        assert_eq!(&*d, "caf");
        let mut d = draft("añ");
        d.left(false, false);
        assert_eq!(d.head(), 1);
    }

    #[test]
    fn arrows_select_and_collapse() {
        let mut d = draft("one two three");
        d.left(true, false);
        assert_eq!(d.head(), 8);
        d.left(true, true);
        assert_eq!(d.selection(), 4..8);
        assert!(d.reversed());
        assert_eq!(d.selected_text(), "two ");
        d.right(false, false);
        assert_eq!((d.head(), d.has_selection()), (8, false));
        d.select(4..7);
        d.left(false, false);
        assert_eq!(d.head(), 4);
        d.right(true, false);
        assert_eq!(d.head(), 7);
        d.select_all();
        d.insert("x");
        assert_eq!(&*d, "x");
    }

    #[test]
    fn home_and_end_stay_on_the_line() {
        let mut d = draft("first\nsecond line\nthird");
        d.move_to(9, false);
        d.home(false);
        assert_eq!(d.head(), 6);
        d.end(true);
        assert_eq!(d.selected_text(), "second line");
        assert_eq!(d.line_at(0), 0..5);
        assert_eq!(d.line_at(d.len()), 18..23);
    }

    #[test]
    fn words_for_double_clicks() {
        let d = draft("fix src/main.rs now");
        assert_eq!(&d[d.word_at(1)], "fix");
        assert_eq!(&d[d.word_at(3)], " ");
        assert_eq!(&d[d.word_at(d.len())], "now");
        assert_eq!(previous_word("cd src/agent/", 13), 7);
        assert_eq!(next_word("a  b c", 1), 4);
    }

    #[test]
    fn token_is_what_is_typed_before_the_caret() {
        let mut d = draft("look at @src/ma and");
        d.move_to(15, false);
        assert_eq!(d.token(), Some((8, "@src/ma")));
        d.move_to(0, false);
        assert_eq!(d.token(), Some((0, "")));
        let d = draft("a\u{a0}@x");
        assert_eq!(d.token(), Some((3, "@x")));
        let mut d = draft("abc");
        d.select_all();
        assert_eq!(d.token(), None);
    }

    #[test]
    fn mentions_get_a_space_before() {
        let mut d = draft("see");
        d.insert_word("@a.rs ");
        assert_eq!(&*d, "see @a.rs ");
        d.insert_word("@b.rs ");
        assert_eq!(&*d, "see @a.rs @b.rs ");
        let mut d = draft("");
        d.insert_word("@c ");
        assert_eq!(&*d, "@c ");
    }

    #[test]
    fn input_methods_compose_then_commit() {
        let mut d = draft("say ");
        d.compose(None, "ni", None);
        assert_eq!((&*d, d.marked()), ("say ni", Some(4..6)));
        d.compose(None, "nih", Some(3..3));
        assert_eq!((&*d, d.marked(), d.head()), ("say nih", Some(4..7), 7));
        d.insert("你好");
        assert_eq!((&*d, d.marked()), ("say 你好", None));
        assert_eq!(d.byte_to_utf16(d.len()), 6);
        assert_eq!(d.utf16_to_byte(5), 7);
        assert_eq!(d.utf16_to_byte(99), d.len());
    }

    fn typed(text: &str) -> Draft {
        let mut d = Draft::default();
        for g in text.graphemes(true) {
            d.insert(g);
        }
        d
    }

    #[test]
    fn undo_takes_back_a_word_at_a_time() {
        let mut d = typed("hola mundo");
        assert!(d.undo());
        assert_eq!(&*d, "hola");
        assert!(d.undo());
        assert_eq!(&*d, "");
        assert!(!d.undo());
        assert!(d.redo() && d.redo());
        assert_eq!((&*d, d.head()), ("hola mundo", 10));
        assert!(!d.redo());
    }

    #[test]
    fn moving_the_caret_or_erasing_starts_a_new_step() {
        let mut d = typed("abc");
        d.move_to(1, false);
        d.insert("x");
        d.backspace(false);
        d.backspace(false);
        assert_eq!(&*d, "bc");
        d.undo();
        assert_eq!((&*d, d.head()), ("axbc", 2));
        d.undo();
        assert_eq!((&*d, d.head()), ("abc", 1));
        d.insert("y");
        assert!(!d.redo(), "a new edit drops what was undone");
        d.select_all();
        d.insert("z");
        d.undo();
        assert_eq!(&*d, "aybc");
    }

    #[test]
    fn composition_undoes_as_one_step() {
        let mut d = typed("say ");
        d.compose(None, "n", None);
        d.compose(None, "ni", None);
        d.insert("你");
        d.undo();
        assert_eq!(&*d, "say ");
    }

    #[test]
    fn restored_drafts_keep_the_caret_and_no_history() {
        let d = Draft::restore("añb".into(), 2);
        assert_eq!(d.head(), 1);
        let mut d = typed("abc");
        assert!(!d.bare().undo());
        d.clear();
        d.undo();
        assert_eq!(&*d, "abc");
    }

    #[test]
    fn offsets_from_outside_land_on_boundaries() {
        let mut d = draft("añb");
        d.move_to(2, false);
        assert_eq!(d.head(), 1);
        d.replace(2..99, "x");
        assert_eq!(&*d, "ax");
    }
}
