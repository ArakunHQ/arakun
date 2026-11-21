//! Finding text in a conversation (Ctrl+F): case-insensitive matches, and
//! what a markdown surface needs to paint them.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{Bounds, Pixels};

/// Where the current match was laid out last, in window coordinates.
pub type Spot = Rc<Cell<Option<Bounds<Pixels>>>>;

/// The query a surface highlights, and which of its matches is the current one.
#[derive(Clone, Copy)]
pub struct Find<'a> {
    pub query: &'a str,
    /// Index among the surface's matches, counted in reading order.
    pub current: Option<usize>,
    pub spot: &'a Spot,
}

/// Non-overlapping matches of `needle` in `hay`, ignoring case.
pub fn find_all(hay: &str, needle: &str) -> Vec<Range<usize>> {
    let needle: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    let mut from = 0;
    for (i, _) in hay.char_indices() {
        if i < from {
            continue;
        }
        if let Some(len) = match_len(&hay[i..], &needle) {
            out.push(i..i + len);
            from = i + len;
        }
    }
    out
}

/// Bytes of `s` that `needle` (lowercased) matches from its start.
fn match_len(s: &str, needle: &[char]) -> Option<usize> {
    let mut want = needle.iter();
    let mut next = want.next();
    for (i, c) in s.char_indices() {
        for l in c.to_lowercase() {
            match next {
                Some(&w) if w == l => next = want.next(),
                _ => return None,
            }
        }
        if next.is_none() {
            return Some(i + c.len_utf8());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_ignore_case_and_do_not_overlap() {
        assert_eq!(find_all("Foo foo FOO", "foo"), vec![0..3, 4..7, 8..11]);
        assert_eq!(find_all("aaaa", "aa"), vec![0..2, 2..4]);
        assert_eq!(find_all("abc", ""), Vec::<Range<usize>>::new());
        assert_eq!(find_all("ab", "abc"), Vec::<Range<usize>>::new());
    }

    #[test]
    fn offsets_are_bytes_of_the_original_text() {
        let hay = "Canción ÁRBOL árbol";
        let hits = find_all(hay, "árbol");
        assert_eq!(hits.len(), 2);
        assert_eq!(&hay[hits[0].clone()], "ÁRBOL");
        assert_eq!(&hay[hits[1].clone()], "árbol");
        assert_eq!(find_all(hay, "CIÓN"), vec![3..8]);
    }
}
