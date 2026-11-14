//! Syntax colors for file and diff tabs, computed once when a document loads.
//! Diff lines keep their `+`/`-`/` ` prefix out of the highlighted text and get
//! their spans shifted past it, so the renderer can tint the marker on its own.

use std::ops::Range;
use std::sync::OnceLock;

use gpui::Hsla;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;

/// Byte ranges of one line with their color.
pub type LineSpans = Vec<(Range<usize>, Hsla)>;

struct Colors {
    syntaxes: SyntaxSet,
    theme: syntect::highlighting::Theme,
}

fn colors() -> Option<&'static Colors> {
    static COLORS: OnceLock<Option<Colors>> = OnceLock::new();
    COLORS
        .get_or_init(|| {
            let mut themes = ThemeSet::load_defaults();
            let theme = themes.themes.remove("base16-ocean.dark")?;
            Some(Colors {
                syntaxes: SyntaxSet::load_defaults_newlines(),
                theme,
            })
        })
        .as_ref()
}

/// Highlights `lines` as the language of `path` (by extension). State runs across
/// the given lines, so a whole file highlights exactly and a diff (a subsequence
/// of file lines) degrades gracefully. None without a known language.
pub fn highlight(path: &str, lines: &[String]) -> Option<Vec<LineSpans>> {
    let colors = colors()?;
    let syntax = colors
        .syntaxes
        .find_syntax_for_file(path)
        .ok()
        .flatten()
        .filter(|s| s.name != "Plain Text")?;
    highlight_with(colors, syntax, lines)
}

/// Highlights a markdown fence by its info string (`rust`, `ts`, `console`...).
/// Untagged and plain-text fences borrow the JavaScript grammar, so strings
/// and numbers in agent output still get colors.
pub fn highlight_lang(lang: &str, lines: &[String]) -> Option<Vec<LineSpans>> {
    let colors = colors()?;
    let lang = lang.trim().to_ascii_lowercase();
    // syntect's default set has no TypeScript or TOML; the nearest grammars read well enough.
    let token = match lang.as_str() {
        "" | "text" | "plaintext" | "txt" | "ts" | "tsx" | "typescript" | "jsx" | "mjs" | "cjs" => {
            "js"
        }
        "console" | "shell" | "zsh" | "fish" | "shellsession" | "terminal" => "sh",
        "jsonc" | "json5" => "json",
        "toml" | "ini" | "cfg" | "conf" => "yaml",
        "c++" => "cpp",
        "csharp" => "cs",
        "py3" | "python3" => "py",
        "golang" => "go",
        "yml" => "yaml",
        "htm" => "html",
        other => other,
    };
    let syntax = colors
        .syntaxes
        .find_syntax_by_token(token)
        .filter(|s| s.name != "Plain Text")?;
    highlight_with(colors, syntax, lines)
}

fn highlight_with(
    colors: &Colors,
    syntax: &syntect::parsing::SyntaxReference,
    lines: &[String],
) -> Option<Vec<LineSpans>> {
    let mut h = HighlightLines::new(syntax, &colors.theme);
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let regions = h.highlight_line(line, &colors.syntaxes).unwrap_or_default();
        let mut spans = LineSpans::new();
        let mut offset = 0;
        for (style, text) in regions {
            let len = text.len();
            if len > 0 && !span_is_plain(colors, style) {
                spans.push((
                    offset..offset + len,
                    gpui::rgb(
                        (style.foreground.r as u32) << 16
                            | (style.foreground.g as u32) << 8
                            | style.foreground.b as u32,
                    )
                    .into(),
                ));
            }
            offset += len;
        }
        out.push(spans);
    }
    Some(out)
}

/// Text drawn in the theme's default foreground keeps the renderer's own color
/// (the diff's green/red), so only styled tokens produce spans.
fn span_is_plain(colors: &Colors, style: syntect::highlighting::Style) -> bool {
    style.font_style == FontStyle::empty()
        && (style.foreground == colors.theme.settings.foreground.unwrap_or(style.foreground))
}

/// The code a diff line feeds the highlighter: without its `+`/`-`/` ` marker,
/// or nothing for `@@` and header lines (they are not code).
pub fn diff_code(line: &str) -> &str {
    match line.as_bytes().first() {
        Some(b'+' | b'-') if !line.starts_with("+++") && !line.starts_with("---") => &line[1..],
        Some(b' ') => &line[1..],
        _ => "",
    }
}

/// Whether a diff line's spans shift one byte to clear the marker.
pub fn diff_shifted(line: &str) -> bool {
    match line.as_bytes().first() {
        Some(b'+' | b'-') => !line.starts_with("+++") && !line.starts_with("---"),
        Some(b' ') => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_markers() {
        assert_eq!(diff_code("+let x = 1;"), "let x = 1;");
        assert_eq!(diff_code("-old();"), "old();");
        assert_eq!(diff_code(" unchanged"), "unchanged");
        assert_eq!(diff_code("+++ b/src/main.rs"), "");
        assert_eq!(diff_code("--- a/src/main.rs"), "");
        assert_eq!(diff_code("@@ -1,2 +1,2 @@"), "");
        assert_eq!(diff_code(""), "");
        assert!(diff_shifted("+let x = 1;"));
        assert!(diff_shifted(" unchanged"));
        assert!(!diff_shifted("+++ b/src/main.rs"));
        assert!(!diff_shifted("@@ -1,2 +1,2 @@"));
    }

    #[test]
    fn highlights_known_syntax() {
        let lines = vec!["fn main() {}".to_string()];
        let spans = highlight("x.rs", &lines).unwrap();
        assert_eq!(spans.len(), 1);
        assert!(!spans[0].is_empty());
    }

    #[test]
    fn highlights_fence_languages() {
        let lines = vec!["let x = \"hi\";".to_string()];
        assert!(highlight_lang("rust", &lines).is_some_and(|s| !s[0].is_empty()));
        assert!(highlight_lang("ts", &lines).is_some_and(|s| !s[0].is_empty()));
        assert!(highlight_lang("", &lines).is_some());
        assert!(highlight_lang("console", &["echo hi".to_string()]).is_some());
        assert!(highlight_lang("nosuchlang", &lines).is_none());
    }

    #[test]
    fn unknown_extension_has_no_spans() {
        let lines = vec!["hello".to_string()];
        assert!(highlight("x.arakunext", &lines).is_none());
    }
}
