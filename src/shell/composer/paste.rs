//! Clipboard for the session composer. Pasted text goes in at the caret, over
//! the selection; copy and cut take the selection, or the whole draft when
//! nothing is selected. Pasted images are saved under arakun's data dir and
//! attached as `@` mentions, the same way the attach picker attaches files.

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, ClipboardEntry, ClipboardItem, Context, InteractiveElement as _, IntoElement as _,
    ObjectFit, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, div, img, px,
};

use super::{Composer, ComposerEvent};
use crate::agent::attach;
use crate::state;
use crate::theme;
use crate::ui::icon;

/// A large paste waiting in the composer. The counts are taken once when it
/// lands, because a paste can be megabytes and the chip shows them every frame.
pub(in crate::shell) struct Paste {
    pub(in crate::shell) text: String,
    pub(in crate::shell) lines: usize,
    pub(in crate::shell) chars: usize,
}

impl Paste {
    pub(super) fn new(text: String) -> Self {
        Self {
            lines: text.lines().count(),
            chars: text.chars().count(),
            text,
        }
    }
}

impl Composer {
    pub(super) fn paste(&mut self, item: Option<ClipboardItem>, cx: &mut Context<Self>) {
        let Some(item) = item else {
            return;
        };
        for entry in item.into_entries() {
            match entry {
                ClipboardEntry::String(s) => {
                    let text = s.text();
                    match pasted_paths(text) {
                        Some(paths) => self.push_mentions(&paths),
                        None if attach::is_large_paste(text) => self
                            .here_mut()
                            .pastes
                            .push(Paste::new(text.replace("\r\n", "\n"))),
                        None => self.here_mut().text.insert(&text.replace("\r\n", "\n")),
                    }
                }
                ClipboardEntry::Image(image) => {
                    match save_image(&image.bytes, image.format.extension()) {
                        Ok(path) => self.push_mentions(&[path]),
                        Err(e) => eprintln!("arakun: could not save the pasted image: {e}"),
                    }
                }
                ClipboardEntry::ExternalPaths(paths) => self.push_mentions(paths.paths()),
            }
        }
        self.changed(cx);
        self.update_completion(cx);
        cx.notify();
    }

    pub(super) fn copy(&mut self, cut: bool, cx: &mut Context<Self>) {
        let draft = &self.here().text;
        let range = if draft.has_selection() {
            draft.selection()
        } else {
            0..draft.len()
        };
        if range.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(draft[range.clone()].to_string()));
        if cut {
            self.here_mut().text.replace(range, "");
            self.update_completion(cx);
        }
        cx.notify();
    }

    /// Puts files in the draft at the caret as `@` mentions, relative to the
    /// environment when they are inside it. Images wait above the text as
    /// pictures instead, and go along with the prompt.
    pub(super) fn push_mentions(&mut self, paths: &[PathBuf]) {
        let root = self.frame.env.clone();
        let env = self.here_mut();
        let (images, files): (Vec<&PathBuf>, Vec<&PathBuf>) = paths
            .iter()
            .partition(|p| attach::mime_type(p).is_some() && p.is_file());
        for image in images {
            if !env.images.contains(image) {
                env.images.push(image.clone());
            }
        }
        let mentions: Vec<String> = files
            .iter()
            .map(|path| {
                attach::mention(&path.strip_prefix(&root).unwrap_or(path).to_string_lossy())
            })
            .collect();
        // All in one edit, so one Ctrl+Z takes them back.
        if !mentions.is_empty() {
            env.text.insert_word(&format!("{} ", mentions.join(" ")));
        }
        self.reveal.set(true);
    }

    /// The images waiting in the composer, as thumbnails: a click shows one
    /// beside the chat, the cross takes it out.
    pub(super) fn render_image_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let images = &self.here().images;
        if images.is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_wrap()
                .gap(crate::ui::sp(8.))
                .px(crate::ui::sp(12.))
                .pt(crate::ui::sp(10.))
                .children(images.iter().enumerate().map(|(i, path)| {
                    let group: SharedString = format!("composer-image-{i}").into();
                    let open = path.clone();
                    div()
                        .id(("composer-image", i))
                        .group(group.clone())
                        .relative()
                        .size(px(64.))
                        .flex_none()
                        .rounded(crate::ui::rad(12.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::fill_hover())
                        .cursor_pointer()
                        .child(
                            img(path.clone())
                                .size_full()
                                .object_fit(ObjectFit::Cover)
                                .with_fallback(|| {
                                    icon("image01", 16.)
                                        .text_color(theme::text_subtle())
                                        .into_any_element()
                                }),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ComposerEvent::OpenImage(open.clone()))
                        }))
                        .child(
                            div()
                                .id(("composer-image-remove", i))
                                .absolute()
                                .top(px(3.))
                                .right(px(3.))
                                .size(px(18.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(crate::ui::rad(8.))
                                .bg(gpui::black().opacity(0.65))
                                .opacity(0.)
                                .group_hover(group, |s| s.opacity(1.))
                                .hover(|s| s.bg(gpui::black().opacity(0.85)))
                                .child(icon("cancel01", 10.).text_color(gpui::white()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let images = &mut this.here_mut().images;
                                    if i < images.len() {
                                        images.remove(i);
                                    }
                                    this.changed(cx);
                                    cx.notify();
                                })),
                        )
                }))
                .into_any_element(),
        )
    }
}
/// Text that is only a list of existing files (what file managers put on the
/// clipboard, as `file://` URIs or plain paths), else `None`.
fn pasted_paths(text: &str) -> Option<Vec<PathBuf>> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    if lines.is_empty() {
        return None;
    }
    lines
        .into_iter()
        .map(|line| {
            let path = match line.strip_prefix("file://") {
                // `file://host/path` is rare; only local URIs are accepted.
                Some(rest) => PathBuf::from(percent_decode(
                    rest.strip_prefix("localhost").unwrap_or(rest),
                )?),
                None => PathBuf::from(line),
            };
            (path.is_absolute() && path.exists()).then_some(path)
        })
        .collect()
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

pub(in crate::shell) fn save_image(bytes: &[u8], ext: &str) -> std::io::Result<PathBuf> {
    let dir = state::data_dir().join("pastes");
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let path = unique(&dir, &format!("paste-{stamp}"), ext);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn unique(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let mut path = dir.join(format!("{stem}.{ext}"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.{ext}"));
        n += 1;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_a_paste_once() {
        let p = Paste::new("añb\nc\n".to_string());
        assert_eq!((p.lines, p.chars), (2, 6));
        assert_eq!(p.text, "añb\nc\n");
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(
            percent_decode("/tmp/My%20Shot%C3%B1.png").unwrap(),
            "/tmp/My Shotñ.png"
        );
        assert_eq!(percent_decode("/bad%2"), None);
    }

    #[test]
    fn reads_copied_files() {
        let dir = std::env::temp_dir().join(format!("arakun-paste {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        std::fs::write(&a, b"x").unwrap();
        let uri = format!("file://{}", a.display()).replace(' ', "%20");
        assert_eq!(pasted_paths(&format!("{uri}\r\n")), Some(vec![a.clone()]));
        assert_eq!(
            pasted_paths(&a.display().to_string()),
            Some(vec![a.clone()])
        );
        assert_eq!(pasted_paths("just some text"), None);
        assert_eq!(pasted_paths(&format!("{}\nnot a file", a.display())), None);
        assert_eq!(pasted_paths("/definitely/missing/file.png"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
