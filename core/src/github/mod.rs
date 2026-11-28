//! GitHub's side of what the app and `arakun server` share. The app reads
//! GitHub through `gh` and keeps its full model; this holds only what a
//! delegated session needs too.

pub mod prompts;

/// A commit as people quote it: its first seven characters. A draft's SHA
/// comes from a file on disk, so it is cut on a character boundary: a byte cut
/// would panic on a damaged file.
pub fn short_sha(sha: &str) -> &str {
    sha.char_indices()
        .nth(7)
        .map_or(sha, |(end, _)| &sha[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_sha_cuts_on_characters() {
        assert_eq!(short_sha("0123456789abcdef"), "0123456");
        assert_eq!(short_sha("abc"), "abc");
        assert_eq!(short_sha(""), "");
        // A damaged draft file: byte 7 falls inside a character.
        assert_eq!(short_sha("abcdeféz"), "abcdefé");
        assert_eq!(short_sha("İİİİİİİİ"), "İİİİİİİ");
        assert_eq!(short_sha("🦀🦀🦀🦀🦀🦀🦀🦀🦀"), "🦀🦀🦀🦀🦀🦀🦀");
    }
}
