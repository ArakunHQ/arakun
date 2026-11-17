//! arakun's own file picker for the composer's "+" button. Two sources, the
//! environment's files and the recent images in Pictures / Downloads / Desktop,
//! plus path browsing when the query starts with `~/`, `/` or `./`. Files are
//! picked one by one into a tray and come back as a list for `@` mentions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Render,
    RenderImage, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    Subscription, UniformListScrollHandle, Window, div, img, px, uniform_list,
};

use crate::agent::attach::{MAX_IMAGE_BYTES, mime_type};
use crate::input::{InputEvent, TextInput};
use crate::model::{age, dir_name, home, pretty_path};
use crate::picker::{fuzzy_score, is_path_query, split_path_query};
use crate::theme;
use crate::ui::{self, ButtonKind, icon};

pub enum AttachEvent {
    Attach(Vec<PathBuf>),
    Dismissed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Project,
    Images,
}

/// The source the last picker was left on; screenshots are the usual pick.
static LAST_WAS_PROJECT: AtomicBool = AtomicBool::new(false);

const ROW_H: f32 = 40.;
const VISIBLE_ROWS: f32 = 9.;
const MAX_ROWS: usize = 400;
/// Twice the 28 px row tile, for HiDPI.
const THUMB_PX: u32 = 64;
const PREVIEW_PX: u32 = 480;
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Folder,
    Image,
    File,
}

#[derive(Clone)]
struct Item {
    path: PathBuf,
    name: SharedString,
    detail: SharedString,
    kind: Kind,
    /// Unix seconds; 0 when unknown.
    modified: u64,
    size: u64,
}

enum Thumb {
    Loading,
    Ready {
        image: Arc<RenderImage>,
        width: u32,
        height: u32,
    },
    Failed,
}

enum Peek {
    Text(Vec<SharedString>),
    Folder(usize),
    Binary,
}

pub struct AttachPicker {
    input: Entity<TextInput>,
    env_name: SharedString,
    source: Source,
    project: Option<Vec<Item>>,
    images: Option<Vec<Item>>,
    rows: Vec<Item>,
    /// Folder being browsed in path mode.
    browsing: Option<PathBuf>,
    cursor: usize,
    chosen: Vec<Item>,
    scroll: UniformListScrollHandle,
    thumbs: HashMap<PathBuf, Thumb>,
    previews: HashMap<PathBuf, Thumb>,
    peek: Option<(PathBuf, Peek)>,
    _input_events: Subscription,
}

impl EventEmitter<AttachEvent> for AttachPicker {}

impl AttachPicker {
    pub fn new(env_path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("Search files or type a path (~/…)", cx));
        let sub = cx.subscribe_in(&input, window, Self::on_input);
        window.focus(&input.focus_handle(cx), cx);
        let source = if LAST_WAS_PROJECT.load(Ordering::Relaxed) {
            Source::Project
        } else {
            Source::Images
        };
        let mut this = Self {
            input,
            env_name: dir_name(&env_path).into(),
            source,
            project: None,
            images: None,
            rows: Vec::new(),
            browsing: None,
            cursor: 0,
            chosen: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            thumbs: HashMap::new(),
            previews: HashMap::new(),
            peek: None,
            _input_events: sub,
        };
        this.refilter(cx);
        cx.spawn(async move |this, cx| {
            let items = cx
                .background_spawn(async move { project_items(&env_path) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.project = Some(items);
                this.refilter(cx);
            });
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let items = cx.background_spawn(async move { recent_images() }).await;
            let _ = this.update(cx, |this, cx| {
                this.images = Some(items);
                this.refilter(cx);
            });
        })
        .detach();
        this
    }

    fn on_input(
        &mut self,
        _: &Entity<TextInput>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Changed => {
                self.cursor = 0;
                self.refilter(cx);
                self.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
            }
            InputEvent::Up => self.move_cursor(-1, cx),
            InputEvent::Down => self.move_cursor(1, cx),
            InputEvent::Submit | InputEvent::SubmitAlt => self.activate(self.cursor, cx),
            InputEvent::Complete => {
                self.toggle(self.cursor, cx);
                if self
                    .rows
                    .get(self.cursor)
                    .is_some_and(|r| r.kind != Kind::Folder)
                {
                    self.move_cursor(1, cx);
                }
            }
            InputEvent::Cancel => self.dismiss(cx),
        }
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).text().to_string()
    }

    fn loading(&self) -> bool {
        self.browsing.is_none()
            && match self.source {
                Source::Project => self.project.is_none(),
                Source::Images => self.images.is_none(),
            }
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        if is_path_query(&query) {
            let (dir, filter) = split_path_query(&query);
            self.rows = list_dir(&dir, &filter);
            self.browsing = Some(dir);
        } else {
            self.browsing = None;
            let items = match self.source {
                Source::Project => self.project.as_deref(),
                Source::Images => self.images.as_deref(),
            }
            .unwrap_or_default();
            self.rows = filter_items(items, &query);
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        cx.notify();
    }

    fn set_source(&mut self, source: Source, window: &mut Window, cx: &mut Context<Self>) {
        LAST_WAS_PROJECT.store(source == Source::Project, Ordering::Relaxed);
        self.source = source;
        self.cursor = 0;
        if self.browsing.is_some() {
            // Leaving path mode: the input's Changed event refilters.
            self.input.update(cx, |i, cx| i.set_text("", cx));
        } else {
            self.refilter(cx);
        }
        self.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
        window.focus(&self.input.focus_handle(cx), cx);
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as isize;
        self.cursor = (self.cursor as isize + delta).rem_euclid(n) as usize;
        self.scroll
            .scroll_to_item(self.cursor, gpui::ScrollStrategy::Nearest);
        cx.notify();
    }

    fn browse_into(&mut self, path: &Path, cx: &mut Context<Self>) {
        let mut text = pretty_path(path);
        if !text.ends_with('/') {
            text.push('/');
        }
        self.input.update(cx, |input, cx| input.set_text(text, cx));
    }

    fn is_chosen(&self, path: &Path) -> bool {
        self.chosen.iter().any(|c| c.path == path)
    }

    /// Tab or a click: put a file in the tray (or take it out); folders open.
    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(item) = self.rows.get(ix).cloned() else {
            return;
        };
        self.cursor = ix;
        if item.kind == Kind::Folder {
            self.browse_into(&item.path, cx);
            return;
        }
        if let Some(i) = self.chosen.iter().position(|c| c.path == item.path) {
            self.chosen.remove(i);
        } else {
            self.chosen.push(item);
        }
        cx.notify();
    }

    /// Enter: attach the tray, or the highlighted file when the tray is empty.
    fn activate(&mut self, ix: usize, cx: &mut Context<Self>) {
        match self.rows.get(ix).cloned() {
            Some(item) if item.kind == Kind::Folder => self.browse_into(&item.path, cx),
            Some(item) if self.chosen.is_empty() => self.finish(vec![item.path], cx),
            _ if !self.chosen.is_empty() => {
                self.finish(self.chosen.iter().map(|c| c.path.clone()).collect(), cx)
            }
            _ => {}
        }
    }

    fn finish(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.release(cx);
        cx.emit(AttachEvent::Attach(paths));
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.release(cx);
        cx.emit(AttachEvent::Dismissed);
    }

    /// Frees the thumbnails' textures; the atlas keeps them until told otherwise.
    pub fn release(&mut self, cx: &mut Context<Self>) {
        for (_, thumb) in self.thumbs.drain().chain(self.previews.drain()) {
            if let Thumb::Ready { image, .. } = thumb {
                cx.drop_image(image, None);
            }
        }
    }

    /// The decoded thumbnail of `path`, starting the decode on first ask.
    fn thumb(&mut self, path: &Path, large: bool, cx: &mut Context<Self>) -> Option<&Thumb> {
        let map = if large {
            &mut self.previews
        } else {
            &mut self.thumbs
        };
        if !map.contains_key(path) {
            if large && map.len() >= 6 {
                // Only the last few previews are worth keeping in the atlas.
                for (_, thumb) in map.drain() {
                    if let Thumb::Ready { image, .. } = thumb {
                        cx.drop_image(image, None);
                    }
                }
            }
            map.insert(path.to_path_buf(), Thumb::Loading);
            let p = path.to_path_buf();
            cx.spawn(async move |this, cx| {
                let decode = p.clone();
                let max = if large { PREVIEW_PX } else { THUMB_PX };
                let result = cx
                    .background_spawn(async move { decode_thumb(&decode, max) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    let map = if large {
                        &mut this.previews
                    } else {
                        &mut this.thumbs
                    };
                    // Dropped by `release` or the preview eviction meanwhile.
                    if !map.contains_key(&p) {
                        return;
                    }
                    map.insert(
                        p,
                        match result {
                            Some((image, width, height)) => Thumb::Ready {
                                image,
                                width,
                                height,
                            },
                            None => Thumb::Failed,
                        },
                    );
                    cx.notify();
                });
            })
            .detach();
        }
        let map = if large { &self.previews } else { &self.thumbs };
        map.get(path)
    }

    fn ensure_peek(&mut self, item: &Item) {
        if self.peek.as_ref().is_some_and(|(p, _)| *p == item.path) {
            return;
        }
        let peek = match item.kind {
            Kind::Folder => {
                Peek::Folder(std::fs::read_dir(&item.path).map_or(0, |r| r.take(1000).count()))
            }
            Kind::Image => return,
            Kind::File => read_text_head(&item.path).map_or(Peek::Binary, Peek::Text),
        };
        self.peek = Some((item.path.clone(), peek));
    }

    // --- rendering -----------------------------------------------------------------------

    fn tile(&mut self, item: &Item, size: f32, cx: &mut Context<Self>) -> AnyElement {
        let base = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(size))
            .rounded(crate::ui::rad(8.))
            .overflow_hidden()
            .bg(theme::fill_hover());
        if item.kind == Kind::Image
            && let Some(Thumb::Ready { image, .. }) = self.thumb(&item.path, false, cx)
        {
            return base
                .child(img(image.clone()).size_full().object_fit(ObjectFit::Cover))
                .into_any_element();
        }
        let color = match item.kind {
            Kind::Folder => theme::mix(0.55),
            Kind::Image => theme::text_muted(),
            Kind::File => theme::mix(0.5),
        };
        base.child(icon(icon_for(item), size * 0.5).text_color(color))
            .into_any_element()
    }

    fn render_row(&mut self, ix: usize, item: &Item, cx: &mut Context<Self>) -> AnyElement {
        let at_cursor = ix == self.cursor;
        let chosen = self.is_chosen(&item.path);
        let tile = self.tile(item, 28., cx);
        let trailing: AnyElement = if item.kind == Kind::Folder {
            icon("arrow-right01", 12.)
                .text_color(theme::text_faint())
                .into_any_element()
        } else if chosen {
            icon("checkmark-circle02", 16.)
                .text_color(theme::text_muted())
                .into_any_element()
        } else {
            icon("circle", 16.)
                .text_color(theme::text_faint())
                .into_any_element()
        };
        let too_big = item.kind == Kind::Image && item.size > MAX_IMAGE_BYTES;
        div()
            .id(("attach-row", ix))
            .w_full()
            .flex()
            .items_center()
            .gap(crate::ui::sp(10.))
            .h(px(ROW_H))
            .px(crate::ui::sp(8.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .cursor_pointer()
            .when(at_cursor, |d| d.bg(theme::selection()))
            .when(!at_cursor && chosen, |d| d.bg(theme::fill_hover()))
            .when(!at_cursor && !chosen, |d| d.hover(|s| s.bg(theme::hover())))
            .child(tile)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(ui::TEXT_MD))
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(item.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .font_family(theme::FONT_MONO)
                            .text_color(theme::text_faint())
                            .truncate()
                            .child(item.detail.clone()),
                    ),
            )
            .when(too_big, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::warn())
                        .child("> 5 MB"),
                )
            })
            .when(item.modified > 0 && item.kind != Kind::Folder, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .child(age(item.modified)),
                )
            })
            .child(
                div()
                    .flex_none()
                    .w(px(16.))
                    .flex()
                    .justify_center()
                    .child(trailing),
            )
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                let Some(item) = this.rows.get(ix).cloned() else {
                    return;
                };
                if ev.click_count() >= 2 && item.kind != Kind::Folder {
                    let mut paths: Vec<PathBuf> =
                        this.chosen.iter().map(|c| c.path.clone()).collect();
                    if !paths.contains(&item.path) {
                        paths.push(item.path);
                    }
                    this.finish(paths, cx);
                } else if ev.click_count() == 1 {
                    this.toggle(ix, cx);
                }
            }))
            .into_any_element()
    }

    fn render_preview(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pane = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(256.))
            .h_full()
            .overflow_hidden()
            .p(crate::ui::sp(12.))
            .gap(crate::ui::sp(10.))
            .border_l_1()
            .border_color(theme::stroke());
        let Some(item) = self.rows.get(self.cursor).cloned() else {
            return pane
                .items_center()
                .justify_center()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_faint())
                .child("Nothing selected")
                .into_any_element();
        };
        self.ensure_peek(&item);
        // The frame takes whatever height the details leave, so nothing spills on short windows.
        let frame = div()
            .flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .w_full()
            .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
            .overflow_hidden()
            .border_1()
            .border_color(theme::stroke())
            .bg(theme::background());
        let mut dims = None;
        let shown: AnyElement = match item.kind {
            Kind::Image => match self.thumb(&item.path, true, cx) {
                Some(Thumb::Ready {
                    image,
                    width,
                    height,
                }) => {
                    dims = Some((*width, *height));
                    frame
                        .p(crate::ui::sp(6.))
                        .child(
                            img(image.clone())
                                .size_full()
                                .object_fit(ObjectFit::Contain),
                        )
                        .into_any_element()
                }
                Some(Thumb::Failed) => frame
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child("Can't preview this image")
                    .into_any_element(),
                _ => frame
                    .child(ui::spinner("attach-preview-spin", 16., theme::mix(0.4)))
                    .into_any_element(),
            },
            _ => match &self.peek {
                Some((_, Peek::Text(lines))) => frame
                    .items_start()
                    .justify_start()
                    .flex_col()
                    .p(crate::ui::sp(8.))
                    .font_family(theme::FONT_MONO)
                    .text_size(px(ui::TEXT_XS))
                    .line_height(px(14.))
                    .text_color(theme::text_muted())
                    .children(
                        lines
                            .iter()
                            .map(|l| div().w_full().truncate().child(l.clone())),
                    )
                    .into_any_element(),
                Some((_, Peek::Folder(n))) => frame
                    .flex_col()
                    .gap(crate::ui::sp(8.))
                    .child(icon("folder-open", 32.).text_color(theme::text_subtle()))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_subtle())
                            .child(match n {
                                0 => "Empty folder".to_string(),
                                1 => "1 item".to_string(),
                                n if *n >= 1000 => "1000+ items".to_string(),
                                n => format!("{n} items"),
                            }),
                    )
                    .into_any_element(),
                _ => frame
                    .flex_col()
                    .gap(crate::ui::sp(8.))
                    .child(icon(icon_for(&item), 32.).text_color(theme::text_subtle()))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child("No preview"),
                    )
                    .into_any_element(),
            },
        };

        let fact = |label: &str, value: String| {
            div()
                .flex()
                .gap(crate::ui::sp(8.))
                .text_size(px(ui::TEXT_XS))
                .child(
                    div()
                        .w(px(72.))
                        .flex_none()
                        .text_color(theme::text_faint())
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme::text_muted())
                        .child(value),
                )
        };
        let mut facts: Vec<gpui::Div> = vec![fact("Type", kind_label(&item))];
        if item.kind != Kind::Folder {
            facts.push(fact("Size", fmt_size(item.size)));
        }
        if let Some((w, h)) = dims {
            facts.push(fact("Dimensions", format!("{w} × {h}")));
        }
        if item.modified > 0 {
            facts.push(fact(
                "Modified",
                format!("{} ago", age(item.modified)).replace("now ago", "just now"),
            ));
        }
        let note: Option<(&str, gpui::Hsla, &str)> = match item.kind {
            Kind::Image if item.size > MAX_IMAGE_BYTES => Some((
                "alert-circle",
                theme::warn().into(),
                "Over 5 MB: only the path is sent.",
            )),
            Kind::Image => Some((
                "image01",
                theme::text_muted(),
                "Sent along with your prompt.",
            )),
            Kind::File => Some(("attachment01", theme::mix(0.5), "Added as an @ mention.")),
            Kind::Folder => None,
        };

        pane.child(shown)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_col()
                    .gap(crate::ui::sp(2.))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_MD))
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(item.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .font_family(theme::FONT_MONO)
                            .text_color(theme::text_faint())
                            .truncate()
                            .child(pretty_path(&item.path)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_col()
                    .gap(crate::ui::sp(4.))
                    .children(facts),
            )
            .when_some(note, |d, (glyph, color, text)| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_start()
                        .gap(crate::ui::sp(6.))
                        .p(crate::ui::sp(8.))
                        .rounded(crate::ui::rad(12.))
                        .bg(theme::fill_hover())
                        .text_size(px(ui::TEXT_XS))
                        .line_height(px(15.))
                        .text_color(theme::text_subtle())
                        .child(icon(glyph, 12.).flex_none().mt(px(1.5)).text_color(color))
                        .child(div().flex_1().min_w_0().child(text)),
                )
            })
            .into_any_element()
    }

    fn render_tray(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.chosen.is_empty() {
            return None;
        }
        let chosen = self.chosen.clone();
        let mut chips = Vec::with_capacity(chosen.len());
        for (i, item) in chosen.iter().enumerate() {
            let tile = self.tile(item, 18., cx);
            let path = item.path.clone();
            chips.push(
                div()
                    .id(("attach-chip", i))
                    .group("attach-chip")
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(crate::ui::sp(6.))
                    .h(px(ui::CONTROL_H))
                    .pl(crate::ui::sp(4.))
                    .pr(crate::ui::sp(2.))
                    .max_w(px(220.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::fill_hover())
                    .text_size(px(ui::TEXT_SM))
                    .child(tile)
                    .child(div().min_w_0().truncate().child(item.name.clone()))
                    .child(
                        div()
                            .id(("attach-chip-x", i))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(20.))
                            .rounded(crate::ui::rad(ui::RADIUS_SM))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::fill_selected()))
                            .child(ui::hover_icon(
                                "cancel01",
                                10.,
                                theme::text_faint(),
                                theme::content(),
                                "attach-chip",
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.chosen.retain(|c| c.path != path);
                                cx.notify();
                            })),
                    ),
            );
        }
        Some(
            div()
                .flex()
                .flex_none()
                .flex_wrap()
                .items_center()
                .gap(crate::ui::sp(6.))
                .px(crate::ui::sp(16.))
                .py(crate::ui::sp(8.))
                .border_t_1()
                .border_color(theme::stroke())
                .children(chips)
                .into_any_element(),
        )
    }
}

impl Focusable for AttachPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for AttachPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport_w = window.viewport_size().width.as_f32();
        let path_mode = self.browsing.is_some();
        let query_empty = self.query(cx).trim().is_empty();
        let section: SharedString = match (&self.browsing, self.source) {
            (Some(dir), _) => pretty_path(dir).into(),
            (None, Source::Project) if query_empty => {
                format!("Recently changed in {}", self.env_name).into()
            }
            (None, Source::Project) => format!("Files in {}", self.env_name).into(),
            (None, Source::Images) if query_empty => {
                "Recent images from Pictures, Downloads and Desktop".into()
            }
            (None, Source::Images) => "Images".into(),
        };
        let count: SharedString = if self.loading() {
            "Loading…".into()
        } else {
            match self.rows.len() {
                1 => "1 item".into(),
                n if n >= MAX_ROWS => format!("{MAX_ROWS}+ items").into(),
                n => format!("{n} items").into(),
            }
        };

        let segment = |id: &'static str,
                       label: &'static str,
                       glyph: &'static str,
                       source: Source,
                       cx: &mut Context<Self>| {
            let active = !path_mode && self.source == source;
            ui::segment(id, Some(glyph), label, active).on_click(
                cx.listener(move |this, _, window, cx| this.set_source(source, window, cx)),
            )
        };
        let segments = ui::segmented()
            .child(segment(
                "attach-src-images",
                "Images",
                "image01",
                Source::Images,
                cx,
            ))
            .child(segment(
                "attach-src-project",
                "Project",
                "folder01",
                Source::Project,
                cx,
            ));

        let field = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(crate::ui::sp(8.))
            .h(px(ui::INPUT_H))
            .px(crate::ui::sp(10.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .border_1()
            .border_color(theme::border_strong())
            .bg(theme::background())
            .text_size(px(ui::TEXT_MD))
            .line_height(px(20.))
            .child(
                icon(if path_mode { "folder-open" } else { "search01" }, 14.)
                    .text_color(theme::text_subtle()),
            )
            .child(self.input.clone());

        let list_h = ROW_H * VISIBLE_ROWS;
        let list: AnyElement = if self.rows.is_empty() {
            let (msg, spin) = if self.loading() {
                (
                    if self.source == Source::Project {
                        "Indexing files…"
                    } else {
                        "Looking for images…"
                    },
                    true,
                )
            } else if path_mode {
                ("Nothing here.", false)
            } else if !query_empty {
                ("Nothing matches. Type a path like ~/ to browse.", false)
            } else if self.source == Source::Images {
                ("No images in Pictures, Downloads or Desktop yet.", false)
            } else {
                ("This environment has no files.", false)
            };
            div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(crate::ui::sp(10.))
                .size_full()
                .px(crate::ui::sp(24.))
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .when(spin, |d| {
                    d.child(ui::spinner("attach-list-spin", 16., theme::mix(0.4)))
                })
                .child(msg)
                .into_any_element()
        } else {
            let rows = self.rows.clone();
            uniform_list(
                "attach-list",
                rows.len(),
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|ix| this.render_row(ix, &rows[ix], cx))
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .size_full()
            .into_any_element()
        };
        let preview = (viewport_w >= 700.).then(|| self.render_preview(cx));
        let tray = self.render_tray(cx);

        let hint = |key: &str, label: &str| {
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(4.))
                .child(
                    div()
                        .px(crate::ui::sp(4.))
                        .h(px(16.))
                        .rounded(crate::ui::rad(ui::RADIUS_SM))
                        .bg(theme::fill_selected())
                        .text_size(px(ui::TEXT_XS))
                        .line_height(px(16.))
                        .text_color(theme::text_subtle())
                        .child(key.to_string()),
                )
                .child(label.to_string())
        };
        let cursor_on_file = self
            .rows
            .get(self.cursor)
            .is_some_and(|r| r.kind != Kind::Folder);
        let attach_label: SharedString = match self.chosen.len() {
            0 => "Attach".into(),
            n => format!("Attach {n}").into(),
        };
        let can_attach = !self.chosen.is_empty() || cursor_on_file;

        ui::modal_panel("attach-picker", 780.)
            .child(ui::modal_header(
                "Attach files",
                Some("Images travel with your prompt; other files are added as @ mentions.".into()),
                ui::close_button("attach-close")
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx))),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    .px(crate::ui::sp(16.))
                    .pt(crate::ui::sp(12.))
                    .child(field)
                    .child(segments),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(crate::ui::sp(20.))
                    .pt(crate::ui::sp(12.))
                    .pb(crate::ui::sp(4.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .child(div().flex_1().min_w_0().truncate().child(section))
                    .child(count),
            )
            .child(
                div()
                    .flex()
                    .h(px(list_h + 8.))
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .px(crate::ui::sp(8.))
                            .pb(crate::ui::sp(8.))
                            .child(list),
                    )
                    .children(preview),
            )
            .children(tray)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .px(crate::ui::sp(16.))
                    .py(crate::ui::sp(10.))
                    .border_t_1()
                    .border_color(theme::stroke())
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .when(viewport_w >= 640., |d| {
                        d.child(hint("↑↓", "navigate"))
                            .child(hint("Tab", "select"))
                            .child(hint("Enter", "attach"))
                    })
                    .child(div().flex_1())
                    .child(
                        ui::button("attach-browse", "Browse…", ButtonKind::Ghost, true).on_click(
                            cx.listener(|this, _, window, cx| {
                                this.input.update(cx, |i, cx| i.set_text("~/", cx));
                                window.focus(&this.input.focus_handle(cx), cx);
                            }),
                        ),
                    )
                    .child(
                        ui::button("attach-ok", attach_label, ButtonKind::Primary, can_attach)
                            .when(can_attach, |b| {
                                b.on_click(
                                    cx.listener(|this, _, _, cx| this.activate(this.cursor, cx)),
                                )
                            }),
                    ),
            )
    }
}

// --- sources ---------------------------------------------------------------------------

fn item_for(path: PathBuf, detail: String, meta: &std::fs::Metadata) -> Item {
    let kind = if meta.is_dir() {
        Kind::Folder
    } else if mime_type(&path).is_some() {
        Kind::Image
    } else {
        Kind::File
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Item {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into(),
        detail: detail.into(),
        kind,
        modified,
        size: meta.len(),
        path,
    }
}

/// The environment's files (git's view, else a bounded walk), newest first.
fn project_items(root: &Path) -> Vec<Item> {
    let files = crate::git::ls_files(root).unwrap_or_else(|_| {
        ignore::WalkBuilder::new(root)
            .max_depth(Some(6))
            .build()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
            .filter_map(|e| {
                e.path()
                    .strip_prefix(root)
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned())
            })
            .take(20_000)
            .collect()
    });
    let mut items: Vec<Item> = files
        .into_iter()
        .filter_map(|rel| {
            let path = root.join(&rel);
            let meta = std::fs::metadata(&path).ok().filter(|m| m.is_file())?;
            let detail = match rel.rfind('/') {
                Some(i) => format!("{}/", &rel[..i]),
                None => "./".to_string(),
            };
            Some(item_for(path, detail, &meta))
        })
        .collect();
    items.sort_by_key(|i| std::cmp::Reverse(i.modified));
    items
}

/// `XDG_<KEY>_DIR` from `~/.config/user-dirs.dirs`, which names them in the user's language.
fn xdg_dir(key: &str) -> Option<PathBuf> {
    let text = std::fs::read_to_string(home().join(".config/user-dirs.dirs")).ok()?;
    parse_user_dir(&text, key, &home())
}

fn parse_user_dir(text: &str, key: &str, home: &Path) -> Option<PathBuf> {
    let prefix = format!("XDG_{key}_DIR=");
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(&prefix))?;
    let value = line[prefix.len()..].trim_matches('"');
    let path = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(value),
    };
    // A dir pointing at $HOME itself means "unset" in xdg-user-dirs.
    (path != home).then_some(path)
}

/// Images in the usual drop spots, newest first.
fn recent_images() -> Vec<Item> {
    let home = home();
    let mut roots: Vec<(PathBuf, usize)> = Vec::new();
    let mut add = |dir: Option<PathBuf>, fallback: &str, depth: usize| {
        let dir = dir.unwrap_or_else(|| home.join(fallback));
        if dir.is_dir() && !roots.iter().any(|(d, _)| *d == dir) {
            roots.push((dir, depth));
        }
    };
    add(xdg_dir("PICTURES"), "Pictures", 3);
    add(xdg_dir("DOWNLOAD"), "Downloads", 2);
    add(xdg_dir("DESKTOP"), "Desktop", 1);
    let mut items: Vec<Item> = Vec::new();
    for (root, depth) in roots {
        let walk = ignore::WalkBuilder::new(&root)
            .max_depth(Some(depth))
            .standard_filters(false)
            .hidden(true)
            .build();
        for entry in walk.filter_map(Result::ok).take(20_000) {
            if !entry.file_type().is_some_and(|t| t.is_file()) || mime_type(entry.path()).is_none()
            {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let path = entry.into_path();
            let detail = path
                .parent()
                .map(|p| format!("{}/", pretty_path(p)))
                .unwrap_or_default();
            items.push(item_for(path, detail, &meta));
        }
    }
    items.sort_by_key(|i| std::cmp::Reverse(i.modified));
    items.truncate(1000);
    items
}

fn filter_items(items: &[Item], query: &str) -> Vec<Item> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return items.iter().take(MAX_ROWS).cloned().collect();
    }
    let mut scored: Vec<(i64, &Item)> = items
        .iter()
        .filter_map(|it| {
            let name = it.name.to_lowercase();
            let full = format!("{}{}", it.detail, it.name).to_lowercase();
            fuzzy_score(&q, &name, &full).map(|s| (s, it))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.modified.cmp(&a.1.modified)));
    scored
        .into_iter()
        .take(MAX_ROWS)
        .map(|(_, it)| it.clone())
        .collect()
}

/// Folders first, then files; prefix matches before the rest.
fn list_dir(dir: &Path, filter: &str) -> Vec<Item> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let f = filter.to_lowercase();
    let show_hidden = filter.starts_with('.');
    let mut rows: Vec<(bool, bool, String, Item)> = read
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && !show_hidden {
                return None;
            }
            let lower = name.to_lowercase();
            if !f.is_empty() && !lower.contains(&f) {
                return None;
            }
            // Follows symlinks, so a linked folder browses like a folder.
            let meta = std::fs::metadata(e.path()).ok()?;
            let item = item_for(e.path(), format!("{}/", pretty_path(dir)), &meta);
            Some((
                item.kind != Kind::Folder,
                !lower.starts_with(&f),
                lower,
                item,
            ))
        })
        .collect();
    rows.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    rows.into_iter().take(2000).map(|(.., item)| item).collect()
}

// --- previews --------------------------------------------------------------------------

fn decode_thumb(path: &Path, max: u32) -> Option<(Arc<RenderImage>, u32, u32)> {
    use image::ImageDecoder as _;
    if std::fs::metadata(path).ok()?.len() > MAX_DECODE_BYTES {
        return None;
    }
    let mut decoder = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = image::DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);
    let (width, height) = (image.width(), image.height());
    let mut buffer = image.thumbnail(max, max).into_rgba8();
    // GPUI textures are BGRA.
    for pixel in buffer.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Some((
        Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])),
        width,
        height,
    ))
}

/// The first lines of a text file, or `None` for binary ones.
fn read_text_head(path: &Path) -> Option<Vec<SharedString>> {
    use std::io::Read as _;
    let mut buf = vec![0u8; 4096];
    let n = std::fs::File::open(path).ok()?.read(&mut buf).ok()?;
    buf.truncate(n);
    if buf.contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(&buf);
    if text.chars().filter(|&c| c == '\u{fffd}').count() > 8 {
        return None;
    }
    Some(
        text.lines()
            .take(11)
            .map(|l| {
                SharedString::from(l.replace('\t', "    ").chars().take(80).collect::<String>())
            })
            .collect(),
    )
}

fn extension(item: &Item) -> String {
    item.path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn icon_for(item: &Item) -> &'static str {
    if item.kind == Kind::Folder {
        return "folder01";
    }
    if item.kind == Kind::Image {
        return "image01";
    }
    match extension(item).as_str() {
        "svg" | "ico" | "bmp" | "tiff" | "avif" | "heic" => "image01",
        "pdf" => "pdf01",
        "mp4" | "mkv" | "webm" | "mov" | "avi" => "video01",
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "opus" => "music-note01",
        "zip" | "tar" | "gz" | "xz" | "zst" | "7z" | "rar" | "bz2" => "zip01",
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "c" | "h" | "cc" | "cpp" | "hpp"
        | "java" | "kt" | "swift" | "rb" | "php" | "sh" | "zsh" | "fish" | "lua" | "toml"
        | "json" | "yaml" | "yml" | "html" | "css" | "scss" | "sql" | "vue" | "svelte" | "zig"
        | "nix" => "file-code",
        _ => "file01",
    }
}

fn kind_label(item: &Item) -> String {
    let ext = extension(item).to_ascii_uppercase();
    match item.kind {
        Kind::Folder => "Folder".into(),
        Kind::Image => format!("{ext} image"),
        Kind::File if ext.is_empty() => "File".into(),
        Kind::File => format!("{ext} file"),
    }
}

fn fmt_size(bytes: u64) -> String {
    const KB: f64 = 1024.;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / KB / KB)
    } else {
        format!("{:.1} GB", b / KB / KB / KB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, detail: &str, modified: u64) -> Item {
        Item {
            path: PathBuf::from(format!("/r/{detail}{name}")),
            name: name.to_string().into(),
            detail: detail.to_string().into(),
            kind: Kind::File,
            modified,
            size: 0,
        }
    }

    #[test]
    fn user_dirs_are_parsed() {
        let text = "# comment\nXDG_PICTURES_DIR=\"$HOME/Imágenes\"\nXDG_DESKTOP_DIR=\"$HOME/\"\nXDG_DOWNLOAD_DIR=\"/data/dl\"\n";
        let home = Path::new("/home/k");
        assert_eq!(
            parse_user_dir(text, "PICTURES", home),
            Some(PathBuf::from("/home/k/Imágenes"))
        );
        assert_eq!(parse_user_dir(text, "DESKTOP", home), None);
        assert_eq!(
            parse_user_dir(text, "DOWNLOAD", home),
            Some(PathBuf::from("/data/dl"))
        );
        assert_eq!(parse_user_dir(text, "MUSIC", home), None);
    }

    #[test]
    fn filtering_ranks_names_first_and_keeps_recency_order() {
        let items = vec![
            item("main.rs", "src/", 30),
            item("shell.rs", "src/", 20),
            item("readme.md", "./", 10),
        ];
        let names = |v: Vec<Item>| {
            v.into_iter()
                .map(|i| i.name.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(filter_items(&items, "")),
            ["main.rs", "shell.rs", "readme.md"]
        );
        assert_eq!(names(filter_items(&items, "she")), ["shell.rs"]);
        assert_eq!(names(filter_items(&items, "src")), ["main.rs", "shell.rs"]);
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(fmt_size(512), "512 B");
        assert_eq!(fmt_size(14 * 1024), "14 KB");
        assert_eq!(fmt_size(2_400_000), "2.3 MB");
    }
}
