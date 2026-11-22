//! Agent markdown: pulldown-cmark parsed into a small block tree, then laid out
//! with GPUI text runs.
//!
//! A streaming message is parsed incrementally: [`Doc::update`] keeps every
//! block that ended before the last two and reparses only the tail, so code
//! blocks earlier in the message keep their syntax colors without redoing them.
//! Every text piece registers in the surface's [`TextMap`] so it can be selected.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClipboardItem, ElementId, FontStyle, FontWeight, Hsla,
    InteractiveElement as _, InteractiveText, IntoElement, ObjectFit, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, StrikethroughStyle, Styled as _,
    StyledImage as _, StyledText, TextRun, UnderlineStyle, Window, div, font, img, px,
};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use super::chart::{self, Diagram};
use super::search::{self, Find};
use super::selection::{Chip, Seg, SelText, TextMap};
use crate::highlight::{self, LineSpans};
use crate::theme;
use crate::ui::{hover_icon, icon};

/// Padding around inline code, as text so the chip wraps with its words. A
/// no-break space never starts or ends a line, so the chip keeps its padding.
const PAD: &str = "\u{a0}";
/// Horizontal padding of a table cell.
const TABLE_CELL_PAD: f32 = 10.;
/// A word longer than this may still wrap: a long path should not make one
/// column take the whole table.
const TABLE_WORD_MAX: f32 = 180.;

/// About how wide the longest unbreakable run of `cell` is at the table's 13 px:
/// monospace advances 7.8 px, sans about 7 px. A chip's NBSP padding counts as
/// part of its word: gpui breaks lines there too, leaving an empty chip behind.
fn longest_word_px(cell: &Inline) -> f32 {
    let mut best = 0f32;
    let mut word = 0f32;
    for (i, ch) in cell.text.char_indices() {
        if ch.is_whitespace() && ch != '\u{a0}' {
            best = best.max(word);
            word = 0.;
            continue;
        }
        let code = cell
            .spans
            .iter()
            .any(|s| s.style.code && s.range.contains(&i));
        word += if code { 7.8 } else { 7. };
    }
    // Rounding in the shaper; a word a pixel short still wraps.
    best.max(word) + 4.
}

/// Room for the icon of a file chip: two monospace cells.
const ICON_PAD: &str = "\u{a0}\u{a0}";
/// An open code block longer than this is shown plain until it closes, so a
/// big block streaming in is not re-highlighted on every chunk.
const LIVE_HIGHLIGHT_LINES: usize = 200;

/// A web link clicked in the chat or a terminal. The shell opens a pull
/// request or issue in the GitHub tab, anything else in a arakun browser tab.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = arakun, no_json)]
pub struct OpenLink {
    pub url: String,
}

/// Opens a link from the chat: web pages in arakun's browser, anything else (`mailto:`…)
/// with the system.
pub fn open_link(url: &str, window: &mut Window, cx: &mut App) {
    if url.starts_with("https://") || url.starts_with("http://") {
        window.dispatch_action(
            Box::new(OpenLink {
                url: url.to_string(),
            }),
            cx,
        );
    } else {
        cx.open_url(url);
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    bold: bool,
    italic: bool,
    code: bool,
    strike: bool,
    /// Chip padding; with `code`, a monospace cell.
    pad: bool,
}

#[derive(Clone)]
struct Span {
    range: Range<usize>,
    style: Style,
    link: Option<SharedString>,
}

#[derive(Clone, Default)]
pub struct Inline {
    text: String,
    spans: Vec<Span>,
    chips: Vec<Chip>,
    pads: Vec<Range<usize>>,
    /// Inline code that names a file, with that name.
    files: Vec<(Range<usize>, String)>,
}

impl Inline {
    fn push(&mut self, s: &str, style: Style, link: Option<&str>) {
        if s.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(s);
        let end = self.text.len();
        if style.pad {
            self.pads.push(start..end);
        }
        if let Some(last) = self.spans.last_mut()
            && last.style == style
            && last.link.as_deref() == link
            && last.range.end == start
        {
            last.range.end = end;
            return;
        }
        self.spans.push(Span {
            range: start..end,
            style,
            link: link.map(|l| SharedString::from(l.to_string())),
        });
    }

    fn code(&mut self, code: &str, style: Style, link: Option<&str>) {
        let file = link.is_none() && file_like(code);
        let start = self.text.len();
        let pad = Style {
            pad: true,
            code: false,
            ..style
        };
        if file {
            self.push(ICON_PAD, Style { code: true, ..pad }, link);
        }
        self.push(PAD, pad, link);
        let lead = self.text.len() - start;
        self.push(
            code,
            Style {
                code: true,
                ..style
            },
            link,
        );
        self.push(PAD, pad, link);
        let icon = match (file, folder_like(code)) {
            (false, _) => None,
            (true, true) => Some(SharedString::from("icons/folder01.svg")),
            (true, false) => Some(SharedString::from("icons/file01.svg")),
        };
        self.chips.push(Chip {
            range: start..self.text.len(),
            lead,
            icon,
        });
        if file {
            self.files.push((start..self.text.len(), code.to_string()));
        }
    }

    pub fn plain(&self) -> &str {
        &self.text
    }

    /// Makes bare URLs in plain text links, as GitHub does.
    fn autolink(&mut self) {
        if !self.text.contains("http") {
            return;
        }
        let mut spans = Vec::with_capacity(self.spans.len());
        for span in std::mem::take(&mut self.spans) {
            if span.link.is_some() || span.style.code || span.style.pad {
                spans.push(span);
                continue;
            }
            let base = span.range.start;
            let mut at = base;
            for url in bare_urls(&self.text[span.range.clone()]) {
                let url = base + url.start..base + url.end;
                if url.start > at {
                    spans.push(Span {
                        range: at..url.start,
                        ..span.clone()
                    });
                }
                let link = SharedString::from(self.text[url.clone()].to_string());
                spans.push(Span {
                    range: url.clone(),
                    style: span.style,
                    link: Some(link),
                });
                at = url.end;
            }
            if at < span.range.end {
                spans.push(Span {
                    range: at..span.range.end,
                    ..span
                });
            }
        }
        self.spans = spans;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone)]
pub enum Block {
    Paragraph(Inline),
    Heading(u8, Inline),
    Code {
        lang: Option<String>,
        /// As the agent wrote it, for the copy button.
        text: String,
        /// Tabs expanded, as shown.
        shown: SharedString,
        spans: Option<Rc<Vec<LineSpans>>>,
        /// What a finished ```chart or ```mermaid block draws.
        diagram: Option<Rc<Diagram>>,
    },
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Quote(Vec<Block>),
    Rule,
    Table {
        align: Vec<Align>,
        rows: Vec<Vec<Inline>>,
    },
    /// `![alt](url)` on its own: a local path (relative to the session's
    /// folder) or a web address.
    Image {
        url: String,
        alt: String,
    },
}

fn code_block(lang: Option<String>, mut text: String, closed: bool) -> Block {
    if text.ends_with('\n') {
        text.pop();
    }
    let shown = text.replace('\t', "    ");
    let lines: Vec<String> = shown.lines().map(str::to_string).collect();
    let spans = (closed || lines.len() <= LIVE_HIGHLIGHT_LINES)
        .then(|| highlight::highlight_lang(lang.as_deref().unwrap_or(""), &lines))
        .flatten()
        .map(Rc::new);
    let diagram = closed
        .then(|| chart::parse(lang.as_deref()?, &text))
        .flatten()
        .map(Rc::new);
    Block::Code {
        lang,
        text,
        shown: shown.into(),
        spans,
        diagram,
    }
}

enum Frame {
    Blocks(Vec<Block>),
    Quote(Vec<Block>),
    List(Option<u64>, Vec<Vec<Block>>),
    Item(Vec<Block>),
    Table(Vec<Align>, Vec<Vec<Inline>>, Vec<Inline>),
}

enum InlineKind {
    Paragraph,
    Heading(u8),
    Cell,
}

struct Builder {
    frames: Vec<Frame>,
    inline: Option<(InlineKind, Inline)>,
    style: Style,
    links: Vec<String>,
    code: Option<(Option<String>, String)>,
    /// An image being read: url, alt text so far, and whether it stands as its
    /// own block (in a paragraph) rather than inline (a heading or a cell).
    image: Option<(String, String, bool)>,
}

impl Builder {
    fn push_block(&mut self, block: Block) {
        match self.frames.last_mut() {
            Some(Frame::Blocks(v) | Frame::Quote(v) | Frame::Item(v)) => v.push(block),
            Some(Frame::List(_, items)) => {
                if items.is_empty() {
                    items.push(Vec::new());
                }
                items.last_mut().unwrap().push(block);
            }
            Some(Frame::Table(..)) | None => {}
        }
    }

    fn flush(&mut self) {
        let Some((kind, mut inline)) = self.inline.take() else {
            return;
        };
        inline.autolink();
        match kind {
            InlineKind::Paragraph if !inline.text.trim().is_empty() => {
                self.push_block(Block::Paragraph(inline))
            }
            InlineKind::Paragraph => {}
            InlineKind::Heading(level) => self.push_block(Block::Heading(level, inline)),
            InlineKind::Cell => {
                if let Some(Frame::Table(_, _, row)) = self.frames.last_mut() {
                    row.push(inline);
                }
            }
        }
    }

    fn inline(&mut self) -> &mut Inline {
        if self.inline.is_none() {
            self.inline = Some((InlineKind::Paragraph, Inline::default()));
        }
        &mut self.inline.as_mut().unwrap().1
    }

    fn text(&mut self, s: &str, style: Style) {
        // Text right after an image starts a new paragraph; its leading space does not.
        if self.inline.is_none() && s.trim().is_empty() {
            return;
        }
        let link = self.links.last().cloned();
        self.inline().push(s, style, link.as_deref());
    }

    /// Top-level blocks finished so far.
    fn top_count(&self) -> usize {
        match self.frames.first() {
            Some(Frame::Blocks(v)) => v.len(),
            _ => 0,
        }
    }
}

/// Where `http(s)://` URLs sit in `text`. A URL ends at whitespace or a quote; trailing
/// punctuation stays out, and so does a closing bracket that has no opening one.
fn bare_urls(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find("http") {
        let start = from + i;
        let rest = &text[start..];
        let scheme = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            from = start + 4;
            continue;
        };
        if text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric())
        {
            from = start + scheme;
            continue;
        }
        let mut end = start
            + rest
                .find(|c: char| {
                    c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`' | '«' | '»' | '“' | '”')
                })
                .unwrap_or(rest.len());
        while let Some(last) = text[start..end].chars().next_back() {
            let url = &text[start..end];
            let trim = match last {
                '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' | '_' | '~' | '’' | '…' => true,
                ')' => url.matches(')').count() > url.matches('(').count(),
                ']' => url.matches(']').count() > url.matches('[').count(),
                _ => false,
            };
            if !trim {
                break;
            }
            end -= last.len_utf8();
        }
        if end > start + scheme {
            out.push(start..end);
        }
        from = end.max(start + scheme);
    }
    out
}

fn starts_block(tag: &Tag) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::BlockQuote(_)
            | Tag::CodeBlock(_)
            | Tag::List(_)
            | Tag::Table(_)
            | Tag::HtmlBlock
    )
}

/// Parses `src`; `marks` gets `(offset, blocks before)` at every top-level block
/// start, offsets relative to `src`.
fn parse_with_marks(src: &str, marks: &mut Vec<(usize, usize)>) -> Vec<Block> {
    let mut b = Builder {
        frames: vec![Frame::Blocks(Vec::new())],
        inline: None,
        style: Style::default(),
        links: Vec::new(),
        code: None,
        image: None,
    };
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for (event, range) in Parser::new_ext(src, opts).into_offset_iter() {
        if let Some((_, code)) = b.code.as_mut() {
            match event {
                Event::Text(t) => {
                    code.push_str(&t);
                    continue;
                }
                Event::End(TagEnd::CodeBlock) => {
                    let (lang, text) = b.code.take().unwrap();
                    b.push_block(code_block(lang, text, true));
                    continue;
                }
                _ => continue,
            }
        }
        if let Some((url, alt, standalone)) = b.image.as_mut() {
            match event {
                Event::Text(t) | Event::Code(t) => alt.push_str(&t),
                Event::End(TagEnd::Image) => {
                    let (url, alt, standalone) =
                        (std::mem::take(url), std::mem::take(alt), *standalone);
                    b.image = None;
                    if standalone {
                        b.push_block(Block::Image { url, alt });
                    } else {
                        let style = b.style;
                        let label = if alt.is_empty() { url.clone() } else { alt };
                        b.inline().push(&label, style, Some(&url));
                    }
                }
                _ => {}
            }
            continue;
        }
        let top = b.frames.len() == 1;
        match event {
            Event::Start(tag) => {
                if top && starts_block(&tag) {
                    b.flush();
                    marks.push((range.start, b.top_count()));
                }
                match tag {
                    Tag::Paragraph => {
                        b.flush();
                        b.inline = Some((InlineKind::Paragraph, Inline::default()));
                    }
                    Tag::Heading { level, .. } => {
                        b.flush();
                        let level = match level {
                            HeadingLevel::H1 => 1,
                            HeadingLevel::H2 => 2,
                            HeadingLevel::H3 => 3,
                            _ => 4,
                        };
                        b.inline = Some((InlineKind::Heading(level), Inline::default()));
                    }
                    Tag::BlockQuote(_) => {
                        b.flush();
                        b.frames.push(Frame::Quote(Vec::new()));
                    }
                    Tag::CodeBlock(kind) => {
                        b.flush();
                        let lang = match kind {
                            CodeBlockKind::Fenced(l) => {
                                Some(l.split_whitespace().next().unwrap_or("").to_string())
                                    .filter(|l| !l.is_empty())
                            }
                            CodeBlockKind::Indented => None,
                        };
                        b.code = Some((lang, String::new()));
                    }
                    Tag::List(start) => {
                        b.flush();
                        b.frames.push(Frame::List(start, Vec::new()));
                    }
                    Tag::Item => {
                        b.flush();
                        b.frames.push(Frame::Item(Vec::new()));
                    }
                    Tag::Table(aligns) => {
                        b.flush();
                        let align = aligns
                            .iter()
                            .map(|a| match a {
                                Alignment::Center => Align::Center,
                                Alignment::Right => Align::Right,
                                _ => Align::Left,
                            })
                            .collect();
                        b.frames.push(Frame::Table(align, Vec::new(), Vec::new()));
                    }
                    Tag::TableHead | Tag::TableRow => {
                        if let Some(Frame::Table(_, _, row)) = b.frames.last_mut() {
                            row.clear();
                        }
                    }
                    Tag::TableCell => b.inline = Some((InlineKind::Cell, Inline::default())),
                    Tag::Emphasis => b.style.italic = true,
                    Tag::Strong => b.style.bold = true,
                    Tag::Strikethrough => b.style.strike = true,
                    Tag::Link { dest_url, .. } => b.links.push(dest_url.to_string()),
                    Tag::Image { dest_url, .. } => {
                        let standalone =
                            matches!(b.inline, None | Some((InlineKind::Paragraph, _)));
                        if standalone {
                            if let Some((_, i)) = b.inline.as_mut() {
                                let n = i.text.trim_end_matches([' ', '\n']).len();
                                i.text.truncate(n);
                                i.spans.retain_mut(|s| {
                                    s.range.end = s.range.end.min(n);
                                    s.range.start < s.range.end
                                });
                            }
                            b.flush();
                        }
                        b.image = Some((dest_url.to_string(), String::new(), standalone));
                    }
                    _ => {}
                }
            }
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) => b.flush(),
                TagEnd::BlockQuote(_) => {
                    b.flush();
                    if let Some(Frame::Quote(v)) = b.frames.pop() {
                        b.push_block(Block::Quote(v));
                    }
                }
                TagEnd::List(_) => {
                    b.flush();
                    if let Some(Frame::List(start, items)) = b.frames.pop() {
                        b.push_block(Block::List { start, items });
                    }
                }
                TagEnd::Item => {
                    b.flush();
                    if let Some(Frame::Item(v)) = b.frames.pop()
                        && let Some(Frame::List(_, items)) = b.frames.last_mut()
                    {
                        items.push(v);
                    }
                }
                TagEnd::TableCell => b.flush(),
                TagEnd::TableHead | TagEnd::TableRow => {
                    if let Some(Frame::Table(_, rows, row)) = b.frames.last_mut() {
                        rows.push(std::mem::take(row));
                    }
                }
                TagEnd::Table => {
                    if let Some(Frame::Table(align, rows, _)) = b.frames.pop() {
                        b.push_block(Block::Table { align, rows });
                    }
                }
                TagEnd::Emphasis => b.style.italic = false,
                TagEnd::Strong => b.style.bold = false,
                TagEnd::Strikethrough => b.style.strike = false,
                TagEnd::Link => {
                    b.links.pop();
                }
                _ => {}
            },
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => {
                let style = b.style;
                b.text(&t, style);
            }
            Event::Code(t) => {
                let style = b.style;
                let link = b.links.last().cloned();
                b.inline().code(&t, style, link.as_deref());
            }
            Event::SoftBreak => {
                let style = b.style;
                b.text(" ", style);
            }
            Event::HardBreak => {
                let style = b.style;
                b.text("\n", style);
            }
            Event::Rule => {
                b.flush();
                if top {
                    marks.push((range.start, b.top_count()));
                }
                b.push_block(Block::Rule);
            }
            Event::TaskListMarker(done) => {
                let style = b.style;
                b.text(if done { "☑ " } else { "☐ " }, style);
            }
            _ => {}
        }
    }
    b.flush();
    if let Some((lang, text)) = b.code.take() {
        b.push_block(code_block(lang, text, false));
    }
    // Close whatever an unfinished stream left open.
    while b.frames.len() > 1 {
        match b.frames.pop().unwrap() {
            Frame::Quote(v) => b.push_block(Block::Quote(v)),
            Frame::List(start, items) => b.push_block(Block::List { start, items }),
            Frame::Item(v) => {
                if let Some(Frame::List(_, items)) = b.frames.last_mut() {
                    items.push(v);
                }
            }
            Frame::Table(align, rows, _) => b.push_block(Block::Table { align, rows }),
            Frame::Blocks(v) => v.into_iter().for_each(|x| b.push_block(x)),
        }
    }
    match b.frames.pop() {
        Some(Frame::Blocks(v)) => v,
        _ => Vec::new(),
    }
}

pub fn parse(src: &str) -> Vec<Block> {
    parse_with_marks(src, &mut Vec::new())
}

fn hash_of(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// A parsed message that can follow its text as it streams in.
#[derive(Clone)]
pub struct Doc {
    pub blocks: Vec<Block>,
    /// `(offset, blocks before)` for every top-level block start.
    marks: Vec<(usize, usize)>,
    /// The text parsed. Comparing against it is a memcmp, far cheaper than
    /// hashing the whole message on every render, and exact where a hash
    /// is not.
    src: String,
}

impl Doc {
    pub fn parse(src: &str) -> Doc {
        let mut marks = Vec::new();
        let blocks = parse_with_marks(src, &mut marks);
        Doc {
            blocks,
            marks,
            src: src.to_string(),
        }
    }

    pub fn is_current(&self, src: &str) -> bool {
        src == self.src
    }

    /// Brings the doc up to `src`. When `src` extends the text parsed before,
    /// only the last two top-level blocks are parsed again: an appended line can
    /// still turn the last paragraph into a heading or a table, but nothing
    /// before a finished block changes.
    pub fn update(&mut self, src: &str) {
        if self.is_current(src) {
            return;
        }
        let extends = src.len() > self.src.len() && src.starts_with(self.src.as_str());
        let keep = if extends && self.marks.len() >= 2 {
            Some(self.marks[self.marks.len() - 2])
        } else {
            None
        };
        let Some((offset, before)) = keep.filter(|&(_, n)| n <= self.blocks.len()) else {
            *self = Doc::parse(src);
            return;
        };
        let mut marks = Vec::new();
        let tail = parse_with_marks(&src[offset..], &mut marks);
        self.blocks.truncate(before);
        self.blocks.extend(tail);
        self.marks.retain(|&(o, _)| o < offset);
        self.marks
            .extend(marks.into_iter().map(|(o, n)| (o + offset, n + before)));
        // Only an extension gets here, so the old text is a prefix of the new.
        let added = &src[self.src.len()..];
        self.src.push_str(added);
    }
}

/// Whether inline code names a file: `src/main.rs`, `Cargo.toml`, `view.rs:120`.
/// Files without an extension that `file_like` still takes for files.
const KNOWN_FILES: &[&str] = &[
    "Makefile",
    "Dockerfile",
    "Containerfile",
    "Justfile",
    "LICENSE",
    "README",
    "CHANGELOG",
    "CODEOWNERS",
    "Gemfile",
    "Rakefile",
    "Procfile",
    "Brewfile",
    "Vagrantfile",
    "Pipfile",
    "go.mod",
    "go.sum",
];

/// A path that names a folder rather than a file, as far as its text tells:
/// `src/browser/`, `/home/u/app`. Dotfiles and extensionless file names stay files.
fn folder_like(s: &str) -> bool {
    let path = file_path(s);
    if path.ends_with('/') {
        return true;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    path.contains('/') && !name.contains('.') && !KNOWN_FILES.contains(&name)
}

pub fn file_like(s: &str) -> bool {
    const EXTS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "jsonc", "md", "mdx", "toml", "yaml",
        "yml", "py", "go", "c", "h", "cc", "cpp", "hpp", "css", "scss", "html", "sh", "bash",
        "zsh", "fish", "txt", "lock", "svg", "png", "jpg", "sql", "java", "kt", "swift", "rb",
        "php", "vue", "svelte", "lua", "zig", "nix", "cfg", "ini", "conf", "xml", "env", "gradle",
        "dart", "ex", "exs", "hs", "ml", "cs", "proto", "graphql", "wgsl", "glsl", "log", "csv",
        "pdf", "desktop", "service",
        // Pictures, media, documents and archives: arakun shows or hands them off.
        "jpeg", "webp", "gif", "bmp", "ico", "tif", "tiff", "avif", "heic", "mp4", "m4v", "mov",
        "webm", "mkv", "avi", "mp3", "wav", "ogg", "opus", "flac", "m4a", "doc", "docx", "xls",
        "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf", "epub", "zip", "tar", "gz", "tgz",
        "bz2", "xz", "zst", "7z", "rar", "ttf", "otf", "woff", "woff2", "psd", "sqlite", "wasm",
        // More text.
        "tsv", "jsonl", "ndjson", "ipynb", "org", "rst", "adoc", "tex", "bib", "scala", "jl", "erl",
        "clj", "elm", "ps1", "bat", "cmd", "mk", "cmake", "tf", "hcl", "prisma", "astro", "gql",
        "plist", "patch", "diff", "pem", "crt", "sass", "less", "styl", "kts", "groovy",
    ];
    /// Dotfiles, which have no extension to go by.
    const DOTFILES: &[&str] = &[
        ".git",
        ".env",
        ".editorconfig",
        ".npmrc",
        ".nvmrc",
        ".prettier",
        ".eslint",
        ".babelrc",
        ".docker",
        ".bash",
        ".zsh",
        ".profile",
        ".vimrc",
        ".tool-versions",
        ".node-version",
        ".python-version",
        ".ruby-version",
        ".stylelint",
        ".swcrc",
        ".envrc",
    ];
    let path = file_path(s);
    if path.is_empty()
        || path.len() > 200
        || path.starts_with("//")
        || !path
            .chars()
            .all(|c| c.is_alphanumeric() || "/._-~@+".contains(c))
    {
        return false;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext_ok = name.rsplit_once('.').is_some_and(|(stem, ext)| {
        EXTS.contains(&ext.to_ascii_lowercase().as_str()) && (!stem.is_empty() || name.len() > 1)
    });
    let known = KNOWN_FILES.contains(&name);
    let dotfile = DOTFILES.iter().any(|d| name.starts_with(d));
    ext_ok || known || dotfile || (path.contains('/') && path.chars().any(char::is_alphabetic))
}

/// Where a markdown link points on this machine: `src/a.rs`, `/abs/b.png`,
/// `file:///x`, `a.rs#L12` (as `a.rs:12`). `None` for web addresses,
/// anchors and other schemes.
pub fn local_link(url: &str) -> Option<String> {
    let url = url.trim().trim_start_matches('<').trim_end_matches('>');
    let path = match url.strip_prefix("file://") {
        Some(rest) => rest,
        None => {
            let scheme = url.split_once(':').map(|(s, _)| s);
            let has_scheme = scheme.is_some_and(|s| {
                s.len() > 1
                    && !s.contains('/')
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
            }) && !url
                .split_once(':')
                .is_some_and(|(_, rest)| rest.starts_with(|c: char| c.is_ascii_digit()));
            if has_scheme || url.starts_with('#') || url.starts_with("www.") || url.is_empty() {
                return None;
            }
            url
        }
    };
    let (path, anchor) = path.split_once('#').unwrap_or((path, ""));
    let path = percent_decode(path.split('?').next().unwrap_or(path));
    if path.is_empty() {
        return None;
    }
    let line = anchor
        .strip_prefix('L')
        .and_then(|l| l.split('-').next())
        .filter(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_digit()));
    Some(match line {
        Some(line) => format!("{path}:{line}"),
        None => path,
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// The path part of a file reference, without a trailing `:line` or `:line:col`.
pub fn file_path(s: &str) -> &str {
    let mut path = s.trim();
    for _ in 0..2 {
        if let Some((head, tail)) = path.rsplit_once(':')
            && !tail.is_empty()
            && tail.chars().all(|c| c.is_ascii_digit())
        {
            path = head;
        }
    }
    path
}

/// The line of a `path:line` or `path:line:col` reference.
pub fn file_line(s: &str) -> Option<usize> {
    let s = s.trim();
    let rest = &s[file_path(s).len()..];
    rest.strip_prefix(':')?
        .split(':')
        .next()?
        .parse()
        .ok()
        .filter(|&n| n > 0)
}

/// Text sizes and colors for one markdown surface (a reply, a thought, ...).
#[derive(Clone, Copy)]
pub struct MdStyle {
    pub size: f32,
    pub line_height: f32,
    pub color: Hsla,
    /// Bold, italic and inline code.
    pub strong: Hsla,
    pub heading: Hsla,
    /// Space between blocks.
    pub gap: f32,
}

impl MdStyle {
    /// Paragraphs at 78% of the content color, emphasis at full.
    pub fn reply() -> Self {
        MdStyle {
            size: 14.,
            line_height: 24.,
            color: theme::mix(0.78),
            strong: theme::content(),
            heading: theme::content(),
            gap: 14.,
        }
    }

    /// Reasoning: 48% text, 62% emphasis.
    pub fn muted() -> Self {
        MdStyle {
            size: 14.,
            line_height: 22.,
            color: theme::mix(0.48),
            strong: theme::mix(0.62),
            heading: theme::mix(0.62),
            gap: 10.,
        }
    }
}

pub(super) fn link_color() -> Hsla {
    Hsla::from(theme::link()).opacity(0.9)
}

thread_local! {
    /// Code blocks whose copy button was pressed, by content, and when.
    static COPIED: RefCell<HashMap<u64, Instant>> = RefCell::new(HashMap::new());
}

const COPIED_FOR: Duration = Duration::from_millis(1500);

fn copied_recently(key: u64) -> bool {
    COPIED.with(|c| {
        let mut c = c.borrow_mut();
        c.retain(|_, at| at.elapsed() < COPIED_FOR);
        c.contains_key(&key)
    })
}

/// Whether `text` went to the clipboard through [`copy_with_feedback`] just now.
pub fn was_copied(text: &str) -> bool {
    copied_recently(hash_of(text))
}

/// Copies `text` and redraws now and once the "copied" moment is over.
pub fn copy_with_feedback(text: &str, window: &mut Window, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
    COPIED.with(|c| c.borrow_mut().insert(hash_of(text), Instant::now()));
    window.refresh();
    window
        .spawn(cx, async move |cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            let _ = cx.update(|window, _| window.refresh());
        })
        .detach();
}

/// A copy button that shows a check for a moment after it is pressed.
pub fn copy_button(id: impl Into<ElementId>, text: String) -> AnyElement {
    copy_button_sized(id, text, 24., 14.)
}

pub fn copy_button_sized(
    id: impl Into<ElementId>,
    text: String,
    box_size: f32,
    icon_size: f32,
) -> AnyElement {
    let key = hash_of(&text);
    let copied = copied_recently(key);
    div()
        .id(id)
        .group("md-copy")
        .size(px(box_size))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(crate::ui::rad(if box_size < 24. { 4. } else { 6. }))
        .cursor_pointer()
        .hover(|s| s.bg(theme::fill_selected()))
        .child(if copied {
            icon("tick02", icon_size)
                .text_color(theme::content())
                .into_any_element()
        } else {
            hover_icon(
                "copy01",
                icon_size,
                theme::text_subtle(),
                theme::content(),
                "md-copy",
            )
            .into_any_element()
        })
        .on_click(move |_, window, cx| copy_with_feedback(&text, window, cx))
        .into_any_element()
}

/// Lays out one surface. Every text piece registers in `map` (cleared first) in
/// reading order; `selection` is a range in the map's numbering.
/// Text that just streamed in, faded by age: `(far, near, opacity)` spans of
/// bytes counted back from the end of the message.
#[derive(Clone, Default)]
pub struct Fade(pub Vec<(usize, usize, f32)>);

/// The paragraph or heading the message currently ends in, where new text lands.
fn last_inline(blocks: &[Block]) -> Option<&Inline> {
    match blocks.last()? {
        Block::Paragraph(i) | Block::Heading(_, i) => Some(i),
        Block::List { items, .. } => last_inline(items.last()?),
        Block::Quote(v) => last_inline(v),
        _ => None,
    }
}

/// `runs` over `text` split where the fade spans start and end, the young
/// parts drawn with less opacity. The spans are mapped from the end of the
/// source to the end of `text`, which differ only by markdown punctuation.
fn fade_runs(runs: Vec<TextRun>, text: &str, fade: &Fade) -> Vec<TextRun> {
    let len = text.len();
    let snap = |mut i: usize| {
        while i < len && !text.is_char_boundary(i) {
            i += 1;
        }
        i
    };
    let spans: Vec<(Range<usize>, f32)> = fade
        .0
        .iter()
        .map(|&(far, near, o)| {
            (
                snap(len.saturating_sub(far))..snap(len.saturating_sub(near)),
                o,
            )
        })
        .filter(|(r, _)| r.start < r.end)
        .collect();
    if spans.is_empty() {
        return runs;
    }
    let mut out = Vec::with_capacity(runs.len() + spans.len() * 2);
    let mut at = 0;
    for run in runs {
        let (a, b) = (at, at + run.len);
        at = b;
        let mut cuts = vec![a, b];
        for (r, _) in &spans {
            for c in [r.start, r.end] {
                if c > a && c < b {
                    cuts.push(c);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let o = spans
                .iter()
                .filter(|(r, _)| r.start <= w[0] && w[1] <= r.end)
                .map(|(_, o)| *o)
                .fold(1f32, f32::min);
            let mut piece = run.clone();
            piece.len = w[1] - w[0];
            piece.color = piece.color.opacity(o);
            out.push(piece);
        }
    }
    out
}

struct Ctx<'a> {
    map: &'a TextMap,
    /// The fade, and the inline it applies to.
    fade: Option<(&'a Fade, *const Inline)>,
    /// The folder relative image paths start from.
    base: Option<&'a Path>,
    /// Copies of images on the server the session runs on, by link.
    fetched: Option<&'a HashMap<String, PathBuf>>,
    selection: Option<Range<usize>>,
    /// Annotated ranges, highlighted until their note is sent or removed.
    marks: &'a [Range<usize>],
    find: Option<Find<'a>>,
    /// Search matches in the pieces laid out so far.
    found: Cell<usize>,
    next: Cell<usize>,
}

impl Ctx<'_> {
    fn piece(
        &self,
        id: ElementId,
        text: SharedString,
        runs: Vec<TextRun>,
        inline: Option<&Inline>,
        links: Vec<(Range<usize>, SharedString)>,
    ) -> AnyElement {
        // Every offset below (runs, links, selection, search) still fits:
        // the masked text keeps its length in bytes.
        let text = crate::redact::same_bytes(&text).map_or(text, SharedString::from);
        let start = self.next.get();
        self.next.set(start + text.len() + 1);
        // Links to files open them in arakun, through the surface's file clicks.
        let (local, links): (Vec<_>, Vec<_>) = links
            .into_iter()
            .partition(|(_, url)| local_link(url).is_some());
        for (r, url) in local {
            if let Some(path) = local_link(&url) {
                self.map.file(start + r.start..start + r.end, path);
            }
        }
        let styled = StyledText::new(text.clone()).with_runs(runs);
        let layout = styled.layout().clone();
        let inner = if links.is_empty() {
            styled.into_any_element()
        } else {
            let ranges: Vec<Range<usize>> = links.iter().map(|(r, _)| r.clone()).collect();
            InteractiveText::new(id, styled)
                .on_click(ranges, move |ix, window, cx| {
                    if let Some((_, url)) = links.get(ix) {
                        open_link(url, window, cx);
                    }
                })
                .into_any_element()
        };
        let bounds = Rc::new(Cell::new(None));
        let (pads, chips) = inline.map_or((Vec::new(), Vec::new()), |i| {
            (i.pads.clone(), i.chips.clone())
        });
        if let Some(i) = inline {
            for (r, path) in &i.files {
                self.map.file(start + r.start..start + r.end, path.clone());
            }
        }
        self.map.push(Seg {
            start,
            text: text.clone(),
            pads,
            layout: layout.clone(),
            bounds: bounds.clone(),
        });
        let end = start + text.len();
        let local = self
            .selection
            .as_ref()
            .filter(|s| s.start < end && s.end > start)
            .map(|s| s.start.max(start) - start..s.end.min(end) - start);
        let marks = self
            .marks
            .iter()
            .filter(|m| m.start < end && m.end > start)
            .map(|m| m.start.max(start) - start..m.end.min(end) - start)
            .collect();
        let sel = SelText::new(
            inner,
            text.clone(),
            layout,
            bounds,
            chips,
            local,
            theme::mix(0.08),
            theme::mix(0.5),
        )
        .marks(marks);
        match self.find {
            Some(f) => {
                let hits = search::find_all(&text, f.query);
                let first = self.found.get();
                self.found.set(first + hits.len());
                let current = f.current.and_then(|c| c.checked_sub(first));
                sel.hits(hits, current, f.spot).into_any_element()
            }
            None => sel.into_any_element(),
        }
    }

    fn inline(
        &self,
        id: ElementId,
        inline: &Inline,
        st: MdStyle,
        weight: FontWeight,
        color: Hsla,
    ) -> AnyElement {
        let mut runs = Vec::with_capacity(inline.spans.len());
        let mut links: Vec<(Range<usize>, SharedString)> = Vec::new();
        for span in &inline.spans {
            let mut f = font(if span.style.code {
                theme::FONT_MONO
            } else {
                theme::FONT_SANS
            });
            f.weight = if span.style.bold {
                FontWeight::SEMIBOLD
            } else {
                weight
            };
            if span.style.italic {
                f.style = FontStyle::Italic;
            }
            let color = if span.link.is_some() {
                link_color()
            } else if span.style.code || span.style.bold || span.style.italic {
                st.strong
            } else {
                color
            };
            runs.push(TextRun {
                len: span.range.len(),
                font: f,
                color,
                background_color: None,
                underline: (span.link.is_some() && !span.style.pad).then(|| UnderlineStyle {
                    thickness: px(1.),
                    color: Some(color.opacity(0.45)),
                    wavy: false,
                }),
                strikethrough: span.style.strike.then_some(StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(color.opacity(0.5)),
                }),
            });
            if let Some(link) = &span.link
                && !span.style.pad
            {
                links.push((span.range.clone(), link.clone()));
            }
        }
        if let Some((fade, target)) = self.fade
            && std::ptr::eq(inline, target)
        {
            runs = fade_runs(runs, &inline.text, fade);
        }
        self.piece(
            id,
            SharedString::from(inline.text.clone()),
            runs,
            Some(inline),
            links,
        )
    }

    fn blocks(&self, blocks: &[Block], st: MdStyle) -> Vec<AnyElement> {
        blocks
            .iter()
            .enumerate()
            .map(|(i, b)| self.block(i, b, st))
            .collect()
    }

    fn block(&self, ix: usize, block: &Block, st: MdStyle) -> AnyElement {
        match block {
            Block::Paragraph(inline) => div()
                .min_w_0()
                .child(self.inline(("p", ix).into(), inline, st, FontWeight::NORMAL, st.color))
                .into_any_element(),
            Block::Heading(level, inline) => {
                let (size, lh) = match level {
                    1 => (22., 30.),
                    2 => (18., 27.),
                    3 => (16., 24.),
                    _ => (14., 22.),
                };
                div()
                    .min_w_0()
                    .when(ix > 0, |d| d.mt(px(6.)))
                    .text_size(px(size))
                    .line_height(px(lh))
                    .child(self.inline(
                        ("h", ix).into(),
                        inline,
                        st,
                        FontWeight::SEMIBOLD,
                        st.heading,
                    ))
                    .into_any_element()
            }
            Block::Code {
                diagram: Some(d),
                text,
                ..
            } => chart::render(ix, d, text),
            Block::Code {
                lang,
                text,
                shown,
                spans,
                ..
            } => self.code(ix, lang.as_deref(), text, shown, spans.as_deref()),
            Block::List { start, items } => div()
                .id(("list", ix))
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .min_w_0()
                .children(items.iter().enumerate().map(|(n, item)| {
                    let marker = match start {
                        Some(s) => format!("{}.", s + n as u64),
                        None => "•".to_string(),
                    };
                    div()
                        .flex()
                        .min_w_0()
                        .child(
                            div()
                                .flex_none()
                                .w(px(24.))
                                .pr(crate::ui::sp(8.))
                                .flex()
                                .justify_end()
                                .text_color(st.color.opacity(0.7))
                                .child(marker),
                        )
                        .child(
                            div()
                                .id(("li", n))
                                .flex()
                                .flex_col()
                                .gap(crate::ui::sp(6.))
                                .flex_1()
                                .min_w_0()
                                .children(self.blocks(item, st)),
                        )
                }))
                .into_any_element(),
            Block::Quote(blocks) => {
                let inner = MdStyle {
                    color: st.color.opacity(0.8),
                    ..st
                };
                div()
                    .id(("quote", ix))
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(8.))
                    .pl(crate::ui::sp(14.))
                    .border_l_2()
                    .border_color(theme::border_strong())
                    .children(self.blocks(blocks, inner))
                    .into_any_element()
            }
            Block::Rule => div()
                .h(px(1.))
                .my(px(4.))
                .bg(theme::fill_selected())
                .into_any_element(),
            Block::Table { align, rows } => self.table(ix, align, rows, st),
            Block::Image { url, alt } => self.image(ix, url, alt, st),
        }
    }

    /// The image at its natural size up to the column width and 560 px tall.
    /// Ctrl+wheel zooms it in place, a click opens it full screen, and dragging
    /// a local one drops it beside the chat. One that cannot be loaded is a link.
    fn image(&self, ix: usize, url: &str, alt: &str, st: MdStyle) -> AnyElement {
        let label = if alt.trim().is_empty() {
            url.to_string()
        } else {
            alt.trim().to_string()
        };
        let link = {
            let (url, label) = (url.to_string(), label.clone());
            move || image_link(&url, &label)
        };
        let Some(source) = image_source(url, self.base, self.fetched) else {
            return link().into_any_element();
        };
        let file = match &source {
            ImageRef::File(p) => Some(p.clone()),
            ImageRef::Web(_) => None,
        };
        let key = match &source {
            ImageRef::File(p) => p.display().to_string(),
            ImageRef::Web(u) => u.to_string(),
        };
        div()
            .id(("img", ix))
            .flex()
            .flex_col()
            .items_start()
            .gap(crate::ui::sp(6.))
            .min_w_0()
            .child(
                div()
                    .id(("img-frame", ix))
                    .relative()
                    .max_w_full()
                    .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::fill_raised())
                    .overflow_hidden()
                    .cursor_pointer()
                    .hover(|s| s.border_color(theme::mix(0.22)))
                    // The plain image sizes the frame; the zoomable one sits over it.
                    .child(
                        img(source.clone())
                            .max_w_full()
                            .max_h(px(560.))
                            .object_fit(ObjectFit::Contain)
                            .opacity(0.)
                            .with_loading(|| div().w(px(240.)).h(px(140.)).into_any_element())
                            .with_fallback(move || link().into_any_element()),
                    )
                    .child(div().absolute().inset_0().child(InlineImage {
                        id: SharedString::from(format!("zoom-inline-{ix}-{key}")),
                        source,
                        file,
                    })),
            )
            .when(!alt.trim().is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(crate::ui::TEXT_SM))
                        .line_height(px(16.))
                        .text_color(st.color.opacity(0.7))
                        .child(label),
                )
            })
            .into_any_element()
    }

    fn table(&self, ix: usize, align: &[Align], rows: &[Vec<Inline>], st: MdStyle) -> AnyElement {
        // Columns share the width by how much they hold, like an auto table layout,
        // but never get narrower than their longest word: a hash or a number split
        // across lines can't be read. Past that the table scrolls sideways.
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
        let weights: Vec<f32> = (0..cols)
            .map(|c| {
                rows.iter()
                    .filter_map(|r| r.get(c))
                    .map(|cell| cell.text.chars().count())
                    .max()
                    .unwrap_or(0)
                    .clamp(4, 48) as f32
            })
            .collect();
        // The cells pad by the Spacing setting, so their minimums must too.
        let pad = f32::from(crate::ui::sp(TABLE_CELL_PAD));
        let mins: Vec<f32> = (0..cols)
            .map(|c| {
                rows.iter()
                    .filter_map(|r| r.get(c))
                    .map(longest_word_px)
                    .fold(0., f32::max)
                    .min(TABLE_WORD_MAX)
                    + pad * 2.
            })
            .collect();
        let min_total: f32 = mins.iter().sum();
        div()
            .id(("table", ix))
            .min_w_0()
            .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .overflow_x_scroll()
            .text_size(px(crate::ui::TEXT_MD))
            .line_height(px(20.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .min_w(px(min_total))
                    .children(rows.iter().enumerate().map(|(r, row)| {
                        div()
                            .id(("tr", r))
                            .flex()
                            .when(r == 0, |d| d.bg(theme::fill_hover()))
                            .when(r > 0, |d| {
                                d.border_t_1().border_color(theme::border_subtle())
                            })
                            .children((0..cols).map(|c| {
                                let weight = weights[c];
                                div()
                                    .flex()
                                    .flex_basis(px(0.))
                                    .min_w(px(mins[c]))
                                    .map(|mut d| {
                                        d.style().flex_grow = Some(weight);
                                        d
                                    })
                                    .px(crate::ui::sp(TABLE_CELL_PAD))
                                    .py(crate::ui::sp(8.))
                                    .when(c > 0, |d| {
                                        d.border_l_1().border_color(theme::border_subtle())
                                    })
                                    // Aligned by the box, not text-align: code chips are
                                    // drawn from the left edge of each line.
                                    .map(|d| match align.get(c) {
                                        Some(Align::Center) => d.justify_center(),
                                        Some(Align::Right) => d.justify_end(),
                                        _ => d,
                                    })
                                    .when_some(row.get(c), |d, cell| {
                                        d.child(div().min_w_0().child(self.inline(
                                            ("td", r * 1000 + c).into(),
                                            cell,
                                            st,
                                            if r == 0 {
                                                FontWeight::SEMIBOLD
                                            } else {
                                                FontWeight::NORMAL
                                            },
                                            if r == 0 { st.strong } else { st.color },
                                        )))
                                    })
                            }))
                    })),
            )
            .into_any_element()
    }

    /// The code block shell: a header with the language and a copy button,
    /// line numbers, and code that scrolls sideways instead of wrapping.
    fn code(
        &self,
        ix: usize,
        lang: Option<&str>,
        text: &str,
        shown: &SharedString,
        spans: Option<&Vec<LineSpans>>,
    ) -> AnyElement {
        let label = lang.unwrap_or("text").to_string();
        let shell = matches!(
            label.as_str(),
            "sh" | "bash" | "zsh" | "fish" | "shell" | "console" | "terminal" | "shellsession"
        );
        let count = shown.lines().count().max(1);
        let numbers = (1..=count)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        // The digits and a little room, plus the right padding, which
        // follows the Spacing setting: a fixed width would squeeze the
        // numbers at a roomier spacing.
        let gutter =
            (px(count.to_string().len() as f32 * 6.5 + 4.) + crate::ui::sp(10.)).max(px(24.));
        let base = theme::mix(0.85);
        let runs = code_runs(shown, spans, base);
        div()
            .id(("code", ix))
            .flex()
            .flex_col()
            .min_w_0()
            // Sunk into the sheet, without a border: the tone sets it apart.
            .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
            .bg(theme::sunk())
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .h(px(36.))
                    .pl(crate::ui::sp(12.))
                    .pr(crate::ui::sp(6.))
                    .child(
                        icon(
                            if shell {
                                "computer-terminal01"
                            } else {
                                "file-code"
                            },
                            14.,
                        )
                        .text_color(theme::text_subtle()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme::FONT_MONO)
                            .text_size(px(crate::ui::TEXT_SM))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text_muted())
                            .child(label),
                    )
                    .child(copy_button(("copy", ix), text.to_string())),
            )
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .py(crate::ui::sp(10.))
                    .font_family(theme::FONT_MONO)
                    .line_height(px(20.))
                    .when(count > 1, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .w(gutter)
                                .pr(crate::ui::sp(10.))
                                .text_right()
                                .text_size(px(crate::ui::TEXT_XS))
                                .text_color(theme::text_faint())
                                .child(numbers),
                        )
                    })
                    .child(
                        div()
                            .id(("code-scroll", ix))
                            .flex_1()
                            .min_w_0()
                            .overflow_x_scroll()
                            .restrict_scroll_to_axis()
                            .when(count <= 1, |d| d.pl(crate::ui::sp(12.)))
                            .child(
                                div()
                                    .flex_none()
                                    .pr(crate::ui::sp(12.))
                                    .whitespace_nowrap()
                                    .text_size(px(crate::ui::TEXT_MD))
                                    .text_color(base)
                                    .child(self.piece(
                                        ("code-text", ix).into(),
                                        shown.clone(),
                                        runs,
                                        None,
                                        Vec::new(),
                                    )),
                            ),
                    ),
            )
            .into_any_element()
    }
}

/// Runs for highlighted code: each line's spans over the base color, the
/// newlines between them plain.
fn code_runs(text: &str, spans: Option<&Vec<LineSpans>>, base: Hsla) -> Vec<TextRun> {
    let f = font(theme::FONT_MONO);
    let run = |len: usize, color: Hsla| TextRun {
        len,
        font: f.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let Some(spans) = spans else {
        return vec![run(text.len(), base)];
    };
    let mut runs = Vec::new();
    let mut offset = 0;
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 {
            runs.push(run(1, base));
        }
        let mut at = 0;
        for (range, color) in spans.get(n).into_iter().flatten() {
            let (a, b) = (range.start.min(line.len()), range.end.min(line.len()));
            if a < at || a >= b {
                continue;
            }
            if a > at {
                runs.push(run(a - at, base));
            }
            runs.push(run(b - a, *color));
            at = b;
        }
        if line.len() > at {
            runs.push(run(line.len() - at, base));
        }
        offset += line.len() + 1;
    }
    let _ = offset;
    runs.retain(|r| r.len > 0);
    if runs.is_empty() {
        runs.push(run(text.len(), base));
    }
    runs
}

/// Lays out `blocks`. `id` must be unique among the caller's siblings; block ids
/// nest under it. The text pieces register in `map` for selection.
#[allow(clippy::too_many_arguments)]
pub fn render(
    id: impl Into<ElementId>,
    blocks: &[Block],
    st: MdStyle,
    map: &TextMap,
    selection: Option<Range<usize>>,
    marks: &[Range<usize>],
    base: Option<&Path>,
    fetched: Option<&HashMap<String, PathBuf>>,
    fade: Option<&Fade>,
    find: Option<Find>,
) -> AnyElement {
    map.clear();
    let ctx = Ctx {
        marks,
        find: find.filter(|f| !f.query.is_empty()),
        found: Cell::new(0),
        map,
        fade: fade
            .filter(|f| !f.0.is_empty())
            .and_then(|f| Some((f, last_inline(blocks)? as *const Inline))),
        base,
        fetched,
        selection,
        next: Cell::new(0),
    };
    let id: ElementId = id.into();
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap(crate::ui::sp(st.gap))
        .min_w_0()
        .text_size(px(st.size))
        .line_height(px(st.line_height))
        .text_color(st.color)
        .font_family(theme::FONT_SANS)
        .children(ctx.blocks(blocks, st))
        .into_any_element()
}

/// The text of every piece [`render`] lays out for `blocks`, in the same order,
/// so matches can be counted without rendering.
pub fn pieces<'a>(blocks: &'a [Block], out: &mut Vec<&'a str>) {
    for block in blocks {
        match block {
            Block::Paragraph(inline) | Block::Heading(_, inline) => out.push(&inline.text),
            Block::Code {
                diagram: Some(_), ..
            } => {}
            Block::Code { shown, .. } => out.push(shown),
            Block::List { items, .. } => items.iter().for_each(|item| pieces(item, out)),
            Block::Quote(inner) => pieces(inner, out),
            Block::Table { rows, .. } => {
                out.extend(rows.iter().flatten().map(|cell| cell.text.as_str()))
            }
            Block::Rule | Block::Image { .. } => {}
        }
    }
}

/// Where an image comes from: a web address as is, a path made absolute.
/// For a session on a server, `fetched` holds the copies of its files, by the
/// url as written: a file at the same path here is another one. `None` for a
/// path with no file (yet).
fn image_source(
    url: &str,
    base: Option<&Path>,
    fetched: Option<&HashMap<String, PathBuf>>,
) -> Option<ImageRef> {
    let clean = url.trim().trim_start_matches('<').trim_end_matches('>');
    if clean.starts_with("http://") || clean.starts_with("https://") {
        return Some(ImageRef::Web(SharedString::from(clean.to_string())));
    }
    match fetched {
        Some(fetched) => fetched.get(url).cloned().map(ImageRef::File),
        None => image_path(url, base)
            .filter(|p| p.is_file())
            .map(ImageRef::File),
    }
}

/// The file an image link names: absolute, `~/` in the home folder, or
/// relative to `base`. `None` for web addresses and data.
pub fn image_path(url: &str, base: Option<&Path>) -> Option<PathBuf> {
    let url = url.trim().trim_start_matches('<').trim_end_matches('>');
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("data:") {
        return None;
    }
    let path = url.strip_prefix("file://").unwrap_or(url);
    let path = match path.strip_prefix("~/") {
        Some(rest) => std::env::home_dir()?.join(rest),
        None => PathBuf::from(path),
    };
    Some(match base {
        Some(base) if path.is_relative() => base.join(path),
        _ => path,
    })
}

/// The links of the images in `src` that name files, as written.
pub fn image_files(src: &str) -> Vec<String> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut out: Vec<String> = Vec::new();
    for event in Parser::new_ext(src, opts) {
        if let Event::Start(Tag::Image { dest_url, .. }) = event
            && image_path(&dest_url, None).is_some()
            && !out.iter().any(|u| *u == *dest_url)
        {
            out.push(dest_url.to_string());
        }
    }
    out
}

#[derive(Clone, PartialEq, Debug)]
pub enum ImageRef {
    File(PathBuf),
    Web(SharedString),
}

impl From<ImageRef> for gpui::ImageSource {
    fn from(r: ImageRef) -> Self {
        match r {
            ImageRef::File(p) => p.into(),
            ImageRef::Web(u) => u.into(),
        }
    }
}

/// A chat image you can zoom in place (it needs the window, so it renders late).
#[derive(IntoElement)]
struct InlineImage {
    id: SharedString,
    source: ImageRef,
    file: Option<PathBuf>,
}

impl RenderOnce for InlineImage {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let open = self.source.clone();
        crate::zoom::zoom_image_inline(
            self.id,
            self.source,
            self.file,
            Some(Rc::new(move |window: &mut Window, _: &mut App| {
                open_preview(open.clone(), window)
            })),
            window,
            cx,
        )
    }
}

fn image_link(url: &str, label: &str) -> gpui::Div {
    let url = url.to_string();
    div().child(
        div()
            .id(SharedString::from(format!("img-link-{url}")))
            .flex()
            .items_center()
            .gap(crate::ui::sp(6.))
            .px(crate::ui::sp(8.))
            .py(crate::ui::sp(4.))
            .rounded(crate::ui::rad(8.))
            .bg(theme::fill_hover())
            .text_size(px(crate::ui::TEXT_MD))
            .text_color(link_color())
            .cursor_pointer()
            .hover(|s| s.bg(theme::fill_selected()))
            .child(icon("image01", 14.).text_color(link_color()))
            .child(div().min_w_0().truncate().child(label.to_string()))
            .on_click(move |_, window, cx| {
                if url.starts_with("http") {
                    open_link(&url, window, cx);
                } else {
                    cx.reveal_path(Path::new(url.strip_prefix("file://").unwrap_or(&url)));
                }
            }),
    )
}

thread_local! {
    /// The image shown full screen over the window, if any.
    static PREVIEW: RefCell<Option<ImageRef>> = const { RefCell::new(None) };
}

pub fn open_preview(image: ImageRef, window: &mut Window) {
    PREVIEW.with(|p| *p.borrow_mut() = Some(image));
    window.refresh();
}

pub fn close_preview(window: &mut Window) -> bool {
    let was = PREVIEW.with(|p| p.borrow_mut().take()).is_some();
    if was {
        window.refresh();
    }
    was
}

pub fn preview() -> Option<ImageRef> {
    PREVIEW.with(|p| p.borrow().clone())
}

/// The full-screen image: dimmed backdrop, the image fit inside the window.
/// The wheel zooms it; a click closes it while it isn't zoomed, and Esc always
/// does (handled by the window root).
pub fn render_preview(image: ImageRef, window: &mut Window, cx: &mut App) -> AnyElement {
    let name = match &image {
        ImageRef::File(p) => p.display().to_string(),
        ImageRef::Web(u) => u.to_string(),
    };
    div()
        .id("image-preview")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(crate::ui::sp(10.))
        .p(crate::ui::sp(32.))
        .bg(gpui::black().opacity(0.82))
        .occlude()
        .cursor_pointer()
        .on_click(|_, window, _| {
            close_preview(window);
        })
        .child(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(crate::zoom::zoom_image(
                    SharedString::from(format!("zoom-preview-{name}")),
                    image.clone(),
                    match &image {
                        ImageRef::File(p) => Some(p.clone()),
                        ImageRef::Web(_) => None,
                    },
                    Some(Rc::new(|window: &mut Window, _: &mut App| {
                        close_preview(window);
                    })),
                    window,
                    cx,
                )),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .text_size(px(crate::ui::TEXT_SM))
                .text_color(gpui::white().opacity(0.6))
                .child(div().max_w(px(600.)).truncate().child(name))
                .child("·")
                .child("Scroll to zoom, drag to move")
                .child("·")
                .child("Esc to close"),
        )
        .into_any_element()
}

/// First line of `text` with markdown punctuation stripped, for one-line summaries.
pub fn summary(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("```"))
        .unwrap_or("");
    let line = line.trim_start_matches(['#', '>', '-', '*', ' ']);
    line.replace("**", "").replace('`', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_files_are_the_images_that_name_files() {
        let text = "![a](shot.png) and ![b](https://x.dev/b.png)\n\n\
            ![c](<my shot.png>) ![d](data:image/png;base64,AA) ![a again](shot.png)\n\n\
            `![e](code.png)` [link](page.png)";
        assert_eq!(image_files(text), ["shot.png", "my shot.png"]);
        let base = Path::new("/srv/repo");
        assert_eq!(
            image_path("shot.png", Some(base)),
            Some(PathBuf::from("/srv/repo/shot.png"))
        );
        assert_eq!(
            image_path("/tmp/a.png", Some(base)),
            Some(PathBuf::from("/tmp/a.png"))
        );
        assert_eq!(image_path("https://x.dev/a.png", Some(base)), None);
    }

    #[test]
    fn a_servers_session_shows_only_the_copies_fetched_for_it() {
        let dir = std::env::temp_dir();
        let name = "arakun-md-fetched-test.png";
        std::fs::write(dir.join(name), b"x").unwrap();
        let copy = PathBuf::from("/tmp/copy.png");
        let fetched = HashMap::from([("shot.png".to_string(), copy.clone())]);
        assert_eq!(
            image_source("shot.png", Some(&dir), Some(&fetched)),
            Some(ImageRef::File(copy))
        );
        // The file of that name here is not the server's.
        assert_eq!(image_source(name, Some(&dir), Some(&fetched)), None);
        assert!(matches!(
            image_source("https://x.io/a.png", Some(&dir), Some(&fetched)),
            Some(ImageRef::Web(_))
        ));
        std::fs::remove_file(dir.join(name)).unwrap();
    }

    #[test]
    fn bare_urls_leave_out_trailing_punctuation() {
        let found = |t: &str| {
            bare_urls(t)
                .into_iter()
                .map(|r| t[r].to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            found("Mira https://aws.amazon.com/startups."),
            ["https://aws.amazon.com/startups"]
        );
        assert_eq!(found("(ver https://a.dev/x)"), ["https://a.dev/x"]);
        assert_eq!(
            found("https://en.wikipedia.org/wiki/Rust_(language)"),
            ["https://en.wikipedia.org/wiki/Rust_(language)"]
        );
        assert_eq!(
            found("dos: http://a.io, https://b.io?q=1"),
            ["http://a.io", "https://b.io?q=1"]
        );
        assert!(found("xhttps://a.io https:// nada").is_empty());
    }

    #[test]
    fn bare_urls_become_links() {
        let blocks = parse("Abre https://example.com/x. o [esto](https://a.io)");
        let Some(Block::Paragraph(inline)) = blocks.first() else {
            panic!("no paragraph")
        };
        let links: Vec<_> = inline
            .spans
            .iter()
            .filter_map(|s| {
                s.link
                    .as_ref()
                    .map(|l| (&inline.text[s.range.clone()], l.to_string()))
            })
            .collect();
        assert_eq!(
            links,
            [
                ("https://example.com/x", "https://example.com/x".to_string()),
                ("esto", "https://a.io".to_string())
            ]
        );
    }

    #[test]
    fn pieces_follow_reading_order() {
        let blocks = parse(
            "# Top\n\n- one\n- two\n\n> said\n\n```\ncode\n```\n\n| a | b |\n|---|---|\n| c | d |",
        );
        let mut out = Vec::new();
        pieces(&blocks, &mut out);
        assert_eq!(
            out,
            ["Top", "one", "two", "said", "code", "a", "b", "c", "d"]
        );
    }

    fn shape(blocks: &[Block]) -> String {
        blocks
            .iter()
            .map(|b| match b {
                Block::Paragraph(i) => format!("p({})", i.text),
                Block::Heading(l, i) => format!("h{l}({})", i.text),
                Block::Code { text, .. } => format!("code({text})"),
                Block::List { items, .. } => format!("list({})", items.len()),
                Block::Quote(v) => format!("quote({})", shape(v)),
                Block::Rule => "rule".into(),
                Block::Table { rows, .. } => format!("table({})", rows.len()),
                Block::Image { url, alt } => format!("img({alt}|{url})"),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn streaming_matches_a_full_parse() {
        let src = "# Title\n\nSome text with `code`.\n\n```rust\nfn main() {}\n```\n\n- a\n- b\n\n| x | y |\n|---|--:|\n| 1 | 2 |\n\nLast para\n===\n\n> quote\n\n---\n\nend";
        let mut doc = Doc::parse("");
        let mut cut = 0;
        while cut < src.len() {
            cut = (cut + 7).min(src.len());
            while !src.is_char_boundary(cut) {
                cut += 1;
            }
            doc.update(&src[..cut]);
            assert_eq!(shape(&doc.blocks), shape(&parse(&src[..cut])), "at {cut}");
        }
    }

    #[test]
    fn a_changed_text_reparses_from_scratch() {
        let mut doc = Doc::parse("one\n\ntwo\n\nthree");
        doc.update("uno\n\ndos\n\ntres\n\ncuatro");
        assert_eq!(shape(&doc.blocks), "p(uno) p(dos) p(tres) p(cuatro)");
    }

    #[test]
    fn same_length_different_text_is_not_current() {
        let mut doc = Doc::parse("one\n\ntwo");
        assert!(doc.is_current("one\n\ntwo"));
        assert!(!doc.is_current("one\n\ntwx"));
        assert!(!doc.is_current("one\n\ntw"));
        doc.update("one\n\ntwx");
        assert!(doc.is_current("one\n\ntwx"));
        assert_eq!(shape(&doc.blocks), "p(one) p(twx)");
    }

    #[test]
    fn an_extension_keeps_the_doc_current() {
        let mut doc = Doc::parse("one\n\ntwo\n\nthr");
        doc.update("one\n\ntwo\n\nthree\n\nfour");
        assert!(doc.is_current("one\n\ntwo\n\nthree\n\nfour"));
        assert_eq!(shape(&doc.blocks), "p(one) p(two) p(three) p(four)");
    }

    #[test]
    fn inline_code_becomes_a_padded_chip() {
        let blocks = parse("see `src/main.rs:12` and `x`");
        let Block::Paragraph(i) = &blocks[0] else {
            panic!()
        };
        assert_eq!(i.chips.len(), 2);
        assert!(i.chips[0].icon.is_some());
        assert!(i.chips[1].icon.is_none());
        assert_eq!(
            i.files,
            vec![(i.files[0].0.clone(), "src/main.rs:12".to_string())]
        );
        let mut plain = i.text.clone();
        for p in i.pads.iter().rev() {
            plain.replace_range(p.clone(), "");
        }
        assert_eq!(plain, "see src/main.rs:12 and x");
    }

    #[test]
    fn file_references() {
        for yes in [
            "src/main.rs",
            "Cargo.toml",
            "view.rs:120",
            "src/agent/",
            "./a/b",
            "Makefile",
            "logo.webp",
            "demo.MP4",
            "informe.docx",
            ".gitignore",
            ".env.local",
            "go.mod",
            "Gemfile",
        ] {
            assert!(file_like(yes), "{yes}");
        }
        for no in [
            "cx.notify",
            "self.open",
            "v1.2.3",
            "std::fs",
            "foo()",
            "a b.rs",
            "https://x.io/a.rs",
            "--release",
            ".map",
            ".len",
            "self.key",
        ] {
            assert!(!file_like(no), "{no}");
        }
        assert_eq!(file_path("src/a.rs:10:2"), "src/a.rs");
        assert_eq!(file_line("src/a.rs:10:2"), Some(10));
        assert_eq!(file_line(" view.rs:120 "), Some(120));
        assert_eq!(file_line("src/a.rs"), None);
        assert_eq!(file_line("src/a.rs:0"), None);
    }

    #[test]
    fn folder_references() {
        for yes in ["src/agent/", "/home/u/proj", "./a/b", "assets/icons"] {
            assert!(folder_like(yes), "{yes}");
        }
        for no in [
            "src/main.rs",
            "Cargo.toml",
            "view.rs:120",
            "docker/Dockerfile",
            "a/.gitignore",
            "Makefile",
        ] {
            assert!(!folder_like(no), "{no}");
        }
    }

    #[test]
    fn links_to_local_files() {
        assert_eq!(local_link("src/main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(
            local_link("/home/k/a%20b.png").as_deref(),
            Some("/home/k/a b.png")
        );
        assert_eq!(
            local_link("file:///tmp/x.pdf").as_deref(),
            Some("/tmp/x.pdf")
        );
        assert_eq!(
            local_link("src/a.rs#L12-L20").as_deref(),
            Some("src/a.rs:12")
        );
        assert_eq!(local_link("view.rs:42").as_deref(), Some("view.rs:42"));
        assert_eq!(
            local_link("<docs/guía.md>").as_deref(),
            Some("docs/guía.md")
        );
        for web in [
            "https://x.io/a.rs",
            "mailto:a@b.c",
            "#section",
            "vscode://file/x",
            "www.x.io",
            "",
        ] {
            assert_eq!(local_link(web), None, "{web}");
        }
    }

    #[test]
    fn images_stand_as_their_own_blocks() {
        let blocks = parse(
            "Here it is:\n![the chart](out/chart.png)\nand more text\n\n# A ![b](c.png) title",
        );
        assert_eq!(
            shape(&blocks),
            "p(Here it is:) img(the chart|out/chart.png) p(and more text) h1(A b title)"
        );
        let streamed = "x\n\n![shot](a.png)\n\nafter";
        let mut doc = Doc::parse("");
        for cut in 1..=streamed.len() {
            doc.update(&streamed[..cut]);
            assert_eq!(
                shape(&doc.blocks),
                shape(&parse(&streamed[..cut])),
                "at {cut}"
            );
        }
    }

    #[test]
    fn image_sources() {
        let dir = std::env::temp_dir();
        let name = "arakun-md-image-test.png";
        std::fs::write(dir.join(name), b"x").unwrap();
        assert_eq!(
            image_source(name, Some(&dir), None),
            Some(ImageRef::File(dir.join(name)))
        );
        assert_eq!(image_source("missing-arakun.png", Some(&dir), None), None);
        assert!(matches!(
            image_source("https://x.io/a.png", None, None),
            Some(ImageRef::Web(_))
        ));
    }

    #[test]
    fn fade_splits_the_young_tail() {
        let run = |len| TextRun {
            len,
            font: font(theme::FONT_SANS),
            color: gpui::white(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let out = fade_runs(
            vec![run(6), run(5)],
            "hello world",
            &Fade(vec![(6, 2, 0.5)]),
        );
        let shape: Vec<(usize, f32)> = out.iter().map(|r| (r.len, r.color.a)).collect();
        assert_eq!(shape, [(5, 1.), (1, 0.5), (3, 0.5), (2, 1.)]);
    }

    #[test]
    fn table_alignment() {
        let blocks = parse("| a | b | c |\n|:-|:-:|-:|\n| 1 | 2 | 3 |");
        let Block::Table { align, rows } = &blocks[0] else {
            panic!()
        };
        assert!(*align == [Align::Left, Align::Center, Align::Right]);
        assert_eq!(rows.len(), 2);
    }
}
