//! How well a row matches what was typed, which letters to highlight, and how
//! often and how lately each thing was opened (frecency), so the palette can
//! put first what the person reaches for.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A match: higher is better, and the byte ranges of `hay` that matched.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub score: i32,
    pub ranges: Vec<Range<usize>>,
}

/// Whether every character of `s` keeps its byte length when lower-cased, so
/// byte offsets found in `s.to_lowercase()` land on the same characters in `s`.
/// Equal total lengths are not enough: the Kelvin sign (3 bytes) lowers to `k`
/// (1 byte) and `İ` (2 bytes) to `i̇` (3 bytes), which can cancel out and leave
/// an offset inside a character, where slicing panics.
pub fn lower_keeps_offsets(s: &str) -> bool {
    s.is_ascii()
        || s.chars()
            .all(|c| c.to_lowercase().map(char::len_utf8).sum::<usize>() == c.len_utf8())
}

fn is_boundary(prev: Option<char>, c: char) -> bool {
    match prev {
        None => true,
        Some(p) => {
            !p.is_alphanumeric() || (p.is_lowercase() && c.is_uppercase()) || p.is_ascii_digit()
        }
    }
}

/// One query word against `hay`: a run of letters scores most at the start
/// of a word, else the letters in order with gaps, at less.
/// `needle` as one piece of `hay` (any case), not letters spread through it.
/// Cheap enough for the long details of every row on every keystroke.
fn substring(hay: &str, needle: &str) -> Option<Hit> {
    if needle.is_empty() {
        return word(hay, needle);
    }
    let needle_l = needle.to_lowercase();
    if let Some(h) = exact(hay, &needle_l) {
        return Some(h);
    }
    if lower_keeps_offsets(hay) {
        return None;
    }
    // `İ` lowers to `i` and a dot, so "ist" is not in "i̇stanbul": compare
    // letter by letter, in one piece.
    word(hay, needle).filter(|h| h.ranges.len() == 1)
}

/// `s` lower-cased, with a way back from byte ranges of the lower text to
/// ranges of `s`, since some letters change length when lowered.
pub struct Lowered<'a> {
    orig: &'a str,
    pub text: String,
    /// Where in `orig` the letter that made each byte of `text` starts.
    from: Vec<usize>,
}

impl<'a> Lowered<'a> {
    pub fn new(orig: &'a str) -> Self {
        let mut text = String::with_capacity(orig.len());
        let mut from = Vec::with_capacity(orig.len());
        for (at, c) in orig.char_indices() {
            for l in c.to_lowercase() {
                text.push(l);
                from.extend(std::iter::repeat_n(at, l.len_utf8()));
            }
        }
        Lowered { orig, text, from }
    }

    /// The range of `orig` that `r` (in `text`) came from. A range that
    /// starts or ends inside one letter's lower case takes the whole letter.
    pub fn back(&self, r: Range<usize>) -> Range<usize> {
        let start = self.from.get(r.start).copied().unwrap_or(self.orig.len());
        let end = match r.end.checked_sub(1).and_then(|e| self.from.get(e)) {
            Some(&at) if r.end > r.start => {
                at + self.orig[at..].chars().next().map_or(0, char::len_utf8)
            }
            _ => start,
        };
        start..end
    }
}

/// The best place `needle_l` (lower case) sits in `hay`, in one piece, with
/// the range in `hay`'s own offsets.
fn exact(hay: &str, needle_l: &str) -> Option<Hit> {
    if lower_keeps_offsets(hay) {
        return best_in(hay, &hay.to_lowercase(), needle_l, |r| r);
    }
    let low = Lowered::new(hay);
    best_in(hay, &low.text, needle_l, |r| low.back(r))
}

/// The best match of `needle_l` in `lower`, `hay` lowercased, with `back`
/// turning a range of `lower` into one of `hay`.
fn best_in(
    hay: &str,
    lower: &str,
    needle_l: &str,
    back: impl Fn(Range<usize>) -> Range<usize>,
) -> Option<Hit> {
    let mut best: Option<Hit> = None;
    for (at, _) in lower.match_indices(needle_l) {
        let r = back(at..at + needle_l.len());
        if r.is_empty() || !hay.is_char_boundary(r.start) || !hay.is_char_boundary(r.end) {
            continue;
        }
        let at = r.start;
        let prev = hay[..at].chars().next_back();
        let c = hay[at..].chars().next().unwrap_or(' ');
        let mut score = 100 + 4 * needle_l.chars().count() as i32;
        if at == 0 {
            score += 60;
        } else if is_boundary(prev, c) {
            score += 40;
        }
        score -= (at as i32).min(30);
        if best.as_ref().is_none_or(|b| score > b.score) {
            best = Some(Hit {
                score,
                ranges: vec![r],
            });
        }
    }
    best
}

fn word(hay: &str, needle: &str) -> Option<Hit> {
    if needle.is_empty() {
        return Some(Hit {
            score: 0,
            ranges: Vec::new(),
        });
    }
    let needle_l = needle.to_lowercase();
    // In one piece first, also when lowering moved offsets: the three-letter
    // rule below is for letters spread out, not for "go" in "Kİİ go".
    if let Some(h) = exact(hay, &needle_l) {
        return Some(h);
    }
    // Letters in order, for three letters or more. Each one prefers the start of a word.
    if needle_l.chars().count() < 3 {
        return None;
    }
    // Letters in order. Each one prefers the start of a word.
    let chars: Vec<(usize, char)> = hay.char_indices().collect();
    let want: Vec<char> = needle_l.chars().collect();
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut score = 0;
    let mut wi = 0;
    let mut last: Option<usize> = None;
    // Every piece starting a word reads as initials (`tsb` for Toggle sidebar).
    let mut initials = true;
    for (ci, &(at, c)) in chars.iter().enumerate() {
        if wi == want.len() {
            break;
        }
        if c.to_lowercase().next() == Some(want[wi]) {
            let prev = ci.checked_sub(1).map(|p| chars[p].1);
            score += if is_boundary(prev, c) { 8 } else { 2 };
            if last == Some(ci.wrapping_sub(1)) {
                score += 5;
                if let Some(r) = ranges.last_mut() {
                    r.end = at + c.len_utf8();
                }
            } else {
                if last.is_some() {
                    score -= 3;
                }
                initials &= is_boundary(prev, c);
                ranges.push(at..at + c.len_utf8());
            }
            last = Some(ci);
            wi += 1;
        }
    }
    // Too scattered to mean anything: two pieces for short words, three past six letters.
    let most = if want.len() > 6 { 3 } else { 2 };
    (wi == want.len() && (initials || ranges.len() <= most)).then_some(Hit { score, ranges })
}

/// Every word of `query` must match `hay`; the score adds up and the ranges
/// merge. An empty query matches everything at 0.
pub fn fuzzy(hay: &str, query: &str) -> Option<Hit> {
    let mut total = Hit {
        score: 0,
        ranges: Vec::new(),
    };
    for w in query.split_whitespace() {
        let hit = word(hay, w)?;
        total.score += hit.score;
        total.ranges.extend(hit.ranges);
    }
    total.ranges = merge(total.ranges);
    Some(total)
}

/// A row's title and its detail: the title must carry the match or the
/// detail can (at a third of the weight); highlights are only in the title.
pub fn score_row(title: &str, detail: &str, query: &str) -> Option<Hit> {
    // As typed, in one piece: a substring match, not letters spread through the text.
    let together = |hay: &str, w: &str| substring(hay, w);
    // Every word as text somewhere: in the title (highlighted) or the detail.
    let mut score = 0;
    let mut ranges = Vec::new();
    let mut whole = true;
    for w in query.split_whitespace() {
        if let Some(h) = together(title, w) {
            score += h.score;
            ranges.extend(h.ranges);
        } else if let Some(h) = together(detail, w) {
            score += h.score / 3;
        } else {
            whole = false;
            break;
        }
    }
    if whole {
        return Some(Hit {
            score,
            ranges: merge(ranges),
        });
    }
    // Else the title's letters in order, for typed-short names (`tsb`).
    fuzzy(title, query)
}

/// Every word of `query` is in `hay` as typed (any case).
pub fn contains_words(hay: &str, query: &str) -> bool {
    let h = hay.to_lowercase();
    query
        .split_whitespace()
        .all(|w| h.contains(&w.to_lowercase()))
}

fn merge(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::new();
    for r in ranges {
        match out.last_mut() {
            Some(l) if r.start <= l.end => l.end = l.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

// ---- frecency ------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Opened {
    pub count: u32,
    /// Unix seconds of the last open.
    pub last: u64,
    /// Opens by hour of the day (local hour as the caller gives it).
    #[serde(default)]
    pub hours: [u16; 24],
}

/// What the person opened from the palette, by a stable key per thing.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Frecency {
    #[serde(default)]
    pub opened: HashMap<String, Opened>,
}

/// Kept small: the least used fall off past this many.
const KEEP: usize = 400;

impl Frecency {
    pub fn record(&mut self, key: &str, now: u64, hour: usize) {
        let e = self.opened.entry(key.to_string()).or_default();
        e.count = e.count.saturating_add(1);
        e.last = now;
        e.hours[hour % 24] = e.hours[hour % 24].saturating_add(1);
        if self.opened.len() > KEEP {
            let mut keys: Vec<(String, f32)> = self
                .opened
                .keys()
                .map(|k| (k.clone(), self.score(k, now, hour)))
                .collect();
            keys.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (k, _) in keys.into_iter().take(self.opened.len() - KEEP) {
                self.opened.remove(&k);
            }
        }
    }

    /// Opens weighted by how lately (halving every three days) plus a lift
    /// for things opened at this hour before.
    pub fn score(&self, key: &str, now: u64, hour: usize) -> f32 {
        let Some(e) = self.opened.get(key) else {
            return 0.;
        };
        let days = now.saturating_sub(e.last) as f32 / 86_400.;
        let recency = 0.5f32.powf(days / 3.);
        let near = |h: usize| e.hours[h % 24] as f32;
        let at_hour = near(hour) + 0.5 * (near(hour + 1) + near(hour + 23));
        (e.count as f32).ln_1p() * 10. * recency + at_hour.min(10.) * 2.
    }

    /// Whether this is what the person tends to open at this hour.
    pub fn usual_at(&self, key: &str, hour: usize) -> bool {
        self.opened
            .get(key)
            .is_some_and(|e| e.hours[hour % 24] >= 2)
    }

    pub fn load() -> Self {
        Self::load_from(&file())
    }

    fn load_from(path: &Path) -> Self {
        crate::stored::load(path).unwrap_or_default()
    }

    pub fn save(&self) {
        self.save_to(&file());
    }

    fn save_to(&self, path: &Path) {
        if let Ok(bytes) = serde_json::to_vec(self) {
            let _ = crate::stored::write(path, &bytes);
        }
    }
}

fn file() -> PathBuf {
    crate::paths::store("palette-frecency.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substring_at_start_beats_middle() {
        let a = fuzzy("Sidebar redesign", "side").unwrap();
        let b = fuzzy("Personalizable sidebar editor", "side").unwrap();
        assert!(a.score > b.score);
        assert_eq!(a.ranges, vec![0..4]);
        assert_eq!(b.ranges, vec![15..19]);
    }

    #[test]
    fn letters_that_change_length_when_lowered_do_not_panic() {
        // Kelvin sign and `İ`: same total length lowered, other boundaries.
        let hay = "\u{212A}\u{130}\u{130}";
        assert_eq!(hay.len(), hay.to_lowercase().len());
        for q in ["i", "k", "ki", "İ"] {
            for h in [fuzzy(hay, q), score_row(hay, hay, q)]
                .into_iter()
                .flatten()
            {
                assert!(h.ranges.iter().all(|r| hay.get(r.clone()).is_some()));
            }
        }
        for hay in ["Café İstanbul 🦀", "🦀 crab ÉTÉ", "\u{212A}elvin é"] {
            for q in ["caf", "é", "ist", "crab", "été", "elvin", "🦀"] {
                for h in [fuzzy(hay, q), score_row(hay, "", q)].into_iter().flatten() {
                    assert!(h.ranges.iter().all(|r| hay.get(r.clone()).is_some()));
                }
            }
        }
        let h = fuzzy("🦀 crab ÉTÉ", "été").unwrap();
        assert_eq!(&"🦀 crab ÉTÉ"[h.ranges[0].clone()], "ÉTÉ");
    }

    #[test]
    fn short_words_match_in_one_piece_when_lowering_moves_offsets() {
        let title = "K\u{130}\u{130} go";
        assert!(!lower_keeps_offsets(title));
        for h in [fuzzy(title, "go"), score_row(title, "", "go")] {
            let h = h.unwrap();
            assert_eq!(h.ranges, vec![6..8]);
            assert_eq!(&title[6..8], "go");
        }
        let h = score_row("Fix \u{130}stanbul", "", "\u{130}st").unwrap();
        assert_eq!(h.ranges, vec![4..8]);
    }

    #[test]
    fn lowered_maps_ranges_back() {
        let s = "\u{130}x\u{212A}y";
        let low = Lowered::new(s);
        assert_eq!(low.text, "i\u{307}xky");
        // "x" sits after the three bytes of "i̇", which came from two.
        assert_eq!(low.back(3..4), 2..3);
        // Half of "i̇" still takes the whole "İ".
        assert_eq!(low.back(0..1), 0..2);
        assert_eq!(low.back(4..6), 3..7);
        assert_eq!(low.back(6..6), 7..7);
    }

    #[test]
    fn case_is_ignored_and_ranges_are_bytes() {
        let h = fuzzy("src/Island.rs", "island").unwrap();
        assert_eq!(h.ranges, vec![4..10]);
    }

    #[test]
    fn letters_in_order_match_at_word_starts() {
        let h = fuzzy("Re-run last check", "rlc").unwrap();
        assert!(h.score > 0);
        assert_eq!(h.ranges, vec![0..1, 7..8, 12..13]);
        assert!(fuzzy("Toggle sidebar", "zz").is_none());
        // Not initials, and three pieces in three letters: noise.
        assert!(fuzzy("Toggle sidebar", "tsb").is_none());
    }

    #[test]
    fn scattered_letters_do_not_match() {
        assert!(fuzzy("abcdefghijklmnop", "aeimo").is_none());
        assert!(fuzzy("src/island_host/call.rs", "install").is_none());
        // Two letters must sit together.
        assert!(fuzzy("New environment", "re").is_none());
        assert!(fuzzy("Restart Arakun", "re").is_some());
    }

    #[test]
    fn every_word_must_match() {
        assert!(fuzzy("island card content", "island card").is_some());
        assert!(fuzzy("island card content", "island zebra").is_none());
        let h = fuzzy("island card", "card island").unwrap();
        assert_eq!(h.ranges, vec![0..6, 7..11]);
    }

    #[test]
    fn detail_counts_less_and_is_not_highlighted() {
        let title = score_row("Notification center", "app · writing tests", "tests").unwrap();
        let both = score_row("tests runner", "", "tests").unwrap();
        assert!(both.score > title.score);
        assert!(title.ranges.is_empty());
        assert!(score_row("Notification center", "app", "zzz").is_none());
    }

    #[test]
    fn non_ascii_titles_do_not_panic() {
        let h = fuzzy("Íslа ünïcode", "ünï").unwrap();
        for r in &h.ranges {
            assert!(
                "Íslа ünïcode".is_char_boundary(r.start) && "Íslа ünïcode".is_char_boundary(r.end)
            );
        }
    }

    #[test]
    fn detail_words_do_not_scatter_into_the_title() {
        // "rik" is the author: the title's r…ik must not light up.
        let h = score_row(
            "chore(spike): prune scripts",
            "o/r #127 rikvisser-dev",
            "rik",
        )
        .unwrap();
        assert!(h.ranges.is_empty());
        assert!(contains_words("src/island_host/cards.rs", "island card"));
        assert!(!contains_words("src/island.rs", "island card"));
    }

    #[test]
    fn frecency_prefers_recent_and_frequent() {
        let mut f = Frecency::default();
        let now = 10 * 86_400;
        f.record("a", now - 9 * 86_400, 9);
        f.record("b", now, 9);
        assert!(f.score("b", now, 9) > f.score("a", now, 9));
        for _ in 0..5 {
            f.record("a", now, 3);
        }
        assert!(f.score("a", now, 9) > f.score("b", now, 9));
        assert_eq!(f.score("missing", now, 9), 0.);
    }

    #[test]
    fn frecency_learns_the_hour() {
        let mut f = Frecency::default();
        let now = 86_400;
        f.record("morning", now, 9);
        f.record("morning", now, 9);
        f.record("night", now, 22);
        f.record("night", now, 22);
        assert!(f.score("morning", now, 9) > f.score("night", now, 9));
        assert!(f.usual_at("morning", 9));
        assert!(!f.usual_at("morning", 22));
    }

    #[test]
    fn frecency_round_trips_through_json() {
        let mut f = Frecency::default();
        f.record("session:abc", 100, 4);
        let back: Frecency = serde_json::from_slice(&serde_json::to_vec(&f).unwrap()).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn frecency_persists_next_to_the_state() {
        let dir = std::env::temp_dir().join(format!("arakun-frecency-{}", std::process::id()));
        let path = dir.join("palette-frecency.json");
        let mut f = Frecency::default();
        f.record("view:github", 50, 1);
        f.save_to(&path);
        let back = Frecency::load_from(&path);
        assert_eq!(back.opened["view:github"].count, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unreadable_frecency_file_is_kept() {
        let dir = crate::stored::tests::scratch("frecency");
        let path = dir.join("palette-frecency.json");
        let torn = r#"{"opened":{"view:github":{"count":1"#;
        std::fs::write(&path, torn).unwrap();
        assert_eq!(Frecency::load_from(&path), Frecency::default());
        Frecency::default().save_to(&path);
        assert_eq!(crate::stored::tests::kept(&path).as_deref(), Some(torn));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn frecency_forgets_the_least_used_past_the_cap() {
        let mut f = Frecency::default();
        for i in 0..(KEEP + 5) {
            f.record(&format!("k{i}"), i as u64, 0);
        }
        assert!(f.opened.len() <= KEEP);
    }
}
