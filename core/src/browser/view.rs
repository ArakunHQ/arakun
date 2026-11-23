//! `BrowserView`: one browser tab (toolbar + page) backed by an off-screen CEF browser.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cef::{
    ImplBrowser as _, ImplBrowserHost as _, ImplFileDialogCallback as _, ImplFrame as _,
    ImplJsdialogCallback as _, KeyEventType,
};
use gpui::{
    AnyElement, App, AppContext as _, Bounds, ClipboardItem, Context, Corners, CursorStyle,
    DispatchPhase, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, HitboxBehavior, InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent,
    Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, Point, Render, RenderImage, ScrollDelta, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, UTF16Selection, Window, canvas,
    div, point, prelude::FluentBuilder as _, px, size,
};

use futures::channel::oneshot;

use super::chrome::{self, Toolbar};
use super::client::{
    self, Bitmap, DevToolsResult, DialogKind, DownloadStatus, FileDialogKind, FileDialogRequest,
    JsDialog, MenuRequest, Tab, TabState, Update,
};
use super::home::{self, HomeInfo};
use super::keys;
use super::runtime;
use super::text_field::{TextField, TextFieldEvent};
use super::url;
use super::{BrowserEvent, ConsoleLine, PageState};
use crate::mascot::Mascot;
use crate::theme;
use crate::ui::icon;

/// "No range" for CEF's IME calls. Passing none means 0..0, which makes Chromium replace from
/// the start of the field: text lands at the beginning instead of at the caret.
const NO_RANGE: cef::Range = cef::Range {
    from: u32::MAX,
    to: u32::MAX,
};

/// Chrome's zoom steps.
const ZOOM_FACTORS: [f64; 13] = [
    0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0,
];

struct Dialog {
    request: JsDialog,
    field: Option<Entity<TextField>>,
    _subscription: Option<Subscription>,
}

#[derive(Clone, Copy)]
enum MenuCommand {
    Back,
    Forward,
    Reload,
    OpenLink,
    CopyLink,
    OpenImage,
    CopyImage,
    Copy,
    Cut,
    Paste,
    SelectAll,
    Inspect,
}

pub struct BrowserView {
    tab: Tab,
    pending_url: Option<String>,
    address: Entity<TextField>,
    /// The big field of the new-tab page.
    home_field: Entity<TextField>,
    /// The new-tab page's raccoon, made when the page shows and dropped after.
    home_mascot: Option<Entity<Mascot>>,
    home: HomeInfo,
    page_focus: FocusHandle,
    dialog_focus: FocusHandle,
    page_bounds: Rc<Cell<Bounds<Pixels>>>,
    image: Option<Arc<RenderImage>>,
    /// Logical size `image` covers (the view size CEF painted it for). Painting the frame at
    /// this size, not at its pixels over the window scale, keeps the page filling the view and
    /// under the mouse even when CEF renders at another density than the window (UI zoom).
    image_view: (i32, i32),
    /// The page's own background, from the last frame. Fills what that frame doesn't
    /// cover while a resized one is on its way, which white made flash on dark pages.
    page_color: gpui::Rgba,
    popup_image: Option<Arc<RenderImage>>,
    popup_rect: Option<Bounds<Pixels>>,
    favicon: Option<Arc<RenderImage>>,
    title: SharedString,
    url: SharedString,
    loading: bool,
    can_back: bool,
    can_forward: bool,
    progress: f64,
    cursor: CursorStyle,
    status: Option<SharedString>,
    selection: String,
    dialog: Option<Dialog>,
    /// Focus to move once a `Window` is at hand (the pump syncs without one).
    pending_focus: Option<FocusHandle>,
    menu: Option<MenuRequest>,
    download: Option<DownloadStatus>,
    error: Option<SharedString>,
    /// CEF could not start; navigating cannot clear this one.
    runtime_error: Option<SharedString>,
    crashed: bool,
    devtools_observer: Option<cef::Registration>,
    visible: bool,
    /// `arakun page` commands in flight; they keep a hidden page rendering.
    holds: u32,
    /// Page loads finished so far.
    loads: u64,
    closed: bool,
    composing: bool,
    dragging: Option<MouseButton>,
    hovered: bool,
    /// The last click or key press, which page-initiated copies must follow closely.
    last_input: Option<Instant>,
    created_at: Instant,
    first_paint: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<BrowserEvent> for BrowserView {}

impl Focusable for BrowserView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.page_focus.clone()
    }
}

impl BrowserView {
    /// Creates a tab. The first one starts CEF (a few tens of ms on the main thread); the
    /// page itself loads asynchronously.
    pub fn new(url: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let created_at = Instant::now();
        let target = if url.trim().is_empty() {
            None
        } else {
            Some(url::from_input(&url))
        };
        let address = cx.new(|cx| TextField::new("Search or enter address", cx));
        let home_field = cx.new(|cx| TextField::new("Search or enter address", cx));
        let error = runtime::ensure(cx).err().map(SharedString::from);
        let tab = TabState::new(1280, 800, window.scale_factor());
        if error.is_none() {
            runtime::register(&tab, cx.weak_entity());
        }
        let page_focus = cx.focus_handle();
        let mut subscriptions = vec![
            cx.subscribe_in(&address, window, Self::address_event),
            cx.subscribe_in(&home_field, window, Self::home_field_event),
            cx.on_focus(&page_focus, window, |this, _, _| {
                if let Some(host) = this.tab.host() {
                    host.set_focus(1);
                }
            }),
            cx.on_blur(&page_focus, window, |this, _, _| {
                if let Some(host) = this.tab.host() {
                    host.set_focus(0);
                }
            }),
            cx.observe_window_activation(window, |this, window, _| {
                if let Some(host) = this.tab.host() {
                    host.set_focus(
                        (window.is_window_active() && this.page_focus.is_focused(window)) as i32,
                    );
                }
            }),
        ];
        subscriptions.push(cx.on_release(|this, cx| {
            for image in [
                this.image.take(),
                this.popup_image.take(),
                this.favicon.take(),
            ]
            .into_iter()
            .flatten()
            {
                cx.drop_image(image, None);
            }
        }));
        let display = target.clone().unwrap_or_default();
        address.update(cx, |field, cx| field.set_text(address_text(&display), cx));
        if target.is_none() {
            window.focus(&home_field.focus_handle(cx), cx);
        }
        Self {
            tab,
            pending_url: Some(target.clone().unwrap_or_else(|| "about:blank".into())),
            address,
            home_field,
            home_mascot: None,
            home: HomeInfo::default(),
            page_focus,
            dialog_focus: cx.focus_handle(),
            page_bounds: Rc::new(Cell::new(Bounds::default())),
            image: None,
            image_view: (0, 0),
            popup_image: None,
            popup_rect: None,
            favicon: None,
            title: SharedString::default(),
            url: target.unwrap_or_default().into(),
            loading: error.is_none(),
            can_back: false,
            can_forward: false,
            progress: 0.,
            cursor: CursorStyle::Arrow,
            status: None,
            selection: String::new(),
            dialog: None,
            pending_focus: None,
            menu: None,
            download: None,
            runtime_error: error.clone(),
            error,
            crashed: false,
            devtools_observer: None,
            visible: true,
            holds: 0,
            loads: 0,
            closed: false,
            composing: false,
            dragging: None,
            last_input: None,
            hovered: false,
            created_at,
            first_paint: false,
            page_color: gpui::rgb(0xffffff),
            _subscriptions: subscriptions,
        }
    }

    pub fn title(&self) -> SharedString {
        match (self.title.as_ref(), self.url.as_ref()) {
            (_, "" | "about:blank") => "New tab".into(),
            ("", url) => url::display(url).into(),
            _ => self.title.clone(),
        }
    }

    pub fn url(&self) -> SharedString {
        self.url.clone()
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn can_go_back(&self) -> bool {
        self.can_back
    }

    pub fn can_go_forward(&self) -> bool {
        self.can_forward
    }

    pub fn favicon(&self) -> Option<Arc<RenderImage>> {
        self.favicon.clone()
    }

    /// The tab shows the new-tab page: it has no address and nothing went wrong. The
    /// first navigation replaces it for good (Back lands on the blank page, as in Chrome).
    pub fn is_home(&self) -> bool {
        !self.closed && self.error.is_none() && matches!(self.url.as_ref(), "" | "about:blank")
    }

    /// The tab shows a raccoon: the new-tab page's.
    pub fn shows_raccoon(&self) -> bool {
        self.is_home()
    }

    /// What the new-tab page says about the environment; the shell keeps it current.
    pub fn set_home(&mut self, info: HomeInfo, cx: &mut Context<Self>) {
        if self.home != info {
            self.home = info;
            cx.notify();
        }
    }

    /// Loads typed text or a URL (see [`url::from_input`]).
    pub fn navigate(&mut self, input: &str, cx: &mut Context<Self>) {
        let target = url::from_input(input);
        self.error = self.runtime_error.clone();
        self.crashed = false;
        self.url = target.clone().into();
        self.address
            .update(cx, |f, cx| f.set_text(address_text(&target), cx));
        match self.tab.browser().and_then(|b| b.main_frame()) {
            Some(frame) => frame.load_url(Some(&target.as_str().into())),
            None => self.pending_url = Some(target),
        }
        cx.notify();
    }

    pub fn back(&mut self, cx: &mut Context<Self>) {
        if let Some(b) = self.tab.browser().filter(|b| b.can_go_back() != 0) {
            b.go_back();
        }
        cx.notify();
    }

    pub fn forward(&mut self, cx: &mut Context<Self>) {
        if let Some(b) = self.tab.browser().filter(|b| b.can_go_forward() != 0) {
            b.go_forward();
        }
        cx.notify();
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.reload_inner(false, cx);
    }

    fn reload_inner(&mut self, ignore_cache: bool, cx: &mut Context<Self>) {
        self.error = self.runtime_error.clone();
        self.crashed = false;
        if let Some(b) = self.tab.browser() {
            if ignore_cache {
                b.reload_ignore_cache()
            } else {
                b.reload()
            }
        }
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(b) = self.tab.browser() {
            b.stop_load();
        }
        cx.notify();
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        self.update_awake();
        if !visible {
            self.menu = None;
            self.status = None;
        }
        cx.notify();
    }

    /// Closes the page (its renderer process goes away). The view then renders empty.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(host) = self.tab.host() {
            host.close_browser(1);
        }
        self.tab.browser.borrow_mut().take();
        for image in [self.image.take(), self.popup_image.take()]
            .into_iter()
            .flatten()
        {
            cx.drop_image(image, None);
        }
        cx.notify();
    }

    /// Gives the address the keyboard: the new-tab page's big field while it shows,
    /// else the toolbar's.
    pub fn focus_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_home() {
            let field = self.home_field.clone();
            window.focus(&field.focus_handle(cx), cx);
            field.update(cx, |f, cx| f.select_all(cx));
            return;
        }
        let field = self.address.clone();
        window.focus(&field.focus_handle(cx), cx);
        field.update(cx, |f, cx| {
            f.set_text(address_text(&self.url), cx);
            f.select_all(cx);
        });
    }

    /// Opens Chromium's DevTools for this page as a new tab (through
    /// [`BrowserEvent::OpenInNewTab`]).
    pub fn inspect(&mut self, cx: &mut Context<Self>) {
        let Some(port) = runtime::cdp_port() else {
            return;
        };
        let info = self.devtools("Target.getTargetInfo", serde_json::json!({}));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(info)) = info.await else { return };
            let Some(id) = info["targetInfo"]["targetId"].as_str() else { return };
            let url = format!("http://127.0.0.1:{port}/devtools/inspector.html?ws=127.0.0.1:{port}/devtools/page/{id}");
            let _ = this.update(cx, |_, cx| cx.emit(BrowserEvent::OpenInNewTab(url)));
        })
        .detach();
    }

    /// Sends a Chrome DevTools Protocol command to the page; resolves with its `result`.
    pub fn devtools(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> oneshot::Receiver<DevToolsResult> {
        if self.devtools_observer.is_none()
            && let Some(host) = self.tab.host()
        {
            let mut observer = client::dev_tools_observer(&self.tab);
            self.devtools_observer = host.add_dev_tools_message_observer(Some(&mut observer));
        }
        self.tab.call(method, params)
    }

    /// What the page logged since it last started loading, oldest first.
    pub fn console(&self) -> Vec<ConsoleLine> {
        self.tab.console.borrow().iter().cloned().collect()
    }

    pub fn page_state(&self) -> PageState {
        PageState {
            open: self.tab.browser.borrow().is_some(),
            loading: self.loading,
            loads: self.loads,
            error: if self.closed {
                Some("the tab was closed".into())
            } else {
                self.error.clone()
            },
            dialog: self.dialog.as_ref().map(|d| d.request.message.clone()),
        }
    }

    /// Keeps the page rendering while it is off screen, so `arakun page` can wait for
    /// loads and take screenshots there. Every `true` needs a matching `false`.
    pub fn hold_awake(&mut self, hold: bool) {
        self.holds = if hold {
            self.holds + 1
        } else {
            self.holds.saturating_sub(1)
        };
        self.update_awake();
    }

    /// Hidden pages stop painting and are throttled by Chromium (`was_hidden`).
    fn update_awake(&mut self) {
        let awake = self.visible || self.holds > 0;
        if self.tab.visible.replace(awake) == awake {
            return;
        }
        if awake {
            runtime::kick();
        }
        if let Some(host) = self.tab.host() {
            host.was_hidden((!awake) as i32);
            if awake {
                host.invalidate(cef::PaintElementType::VIEW);
            }
        }
    }

    // --- CEF → view ------------------------------------------------------------------------

    /// Called by the pump after CEF did work for this tab.
    pub(super) fn sync(&mut self, cx: &mut Context<Self>) {
        let tab = self.tab.clone();
        tab.dirty.set(false);
        if self.closed {
            tab.frame.borrow_mut().take();
            tab.popup_frame.borrow_mut().take();
            tab.updates.borrow_mut().clear();
            return;
        }
        if tab.browser.borrow().is_none()
            && runtime::CONTEXT_READY.load(std::sync::atomic::Ordering::Relaxed)
        {
            let url = self
                .pending_url
                .take()
                .unwrap_or_else(|| "about:blank".into());
            let browser = client::create_browser(&url, &tab);
            if browser.is_none() {
                self.error = Some("Chromium could not create the page".into());
            }
            *tab.browser.borrow_mut() = browser;
            if let Some(host) = tab.host()
                && !tab.visible.get()
            {
                host.was_hidden(1);
            }
        }

        if tab.live_changed.replace(false)
            && let Some(image) = tab.live.borrow().clone()
            && let Some(old) = self.image.replace(image)
        {
            cx.drop_image(old, None);
        }
        if tab.updated.replace(false) {
            if !self.first_paint {
                self.first_paint = true;
            }
            self.image_view = tab.frame_view.get();
            if let Some([r, g, b]) = tab.edge.take() {
                self.page_color = gpui::rgb(u32::from_be_bytes([0, r, g, b]));
            }
        }
        let frame = tab.frame.borrow_mut().take();
        if let Some(frame) = frame {
            if !self.first_paint {
                self.first_paint = true;
                if runtime::timing() {
                    eprintln!(
                        "arakun-browser: first paint {:?} after BrowserView::new",
                        self.created_at.elapsed()
                    );
                }
            }
            self.image_view = tab.frame_view.get();
            self.replace_image(frame, false, cx);
        }
        let popup = tab.popup_frame.borrow_mut().take();
        if let Some(popup) = popup {
            self.replace_image(popup, true, cx);
        }

        let updates = std::mem::take(&mut *tab.updates.borrow_mut());
        for update in updates {
            self.apply(update, cx);
        }
        cx.notify();
    }

    fn replace_image(&mut self, bitmap: Bitmap, popup: bool, cx: &mut Context<Self>) {
        if !popup
            && let Some([r, g, b]) = super::client::edge_color(
                &bitmap.bgra,
                bitmap.width as usize,
                bitmap.height as usize,
            )
        {
            self.page_color = gpui::rgb(u32::from_be_bytes([0, r, g, b]));
        }
        let Some(image) = to_render_image(bitmap) else {
            return;
        };
        if !popup {
            // The next frame of this size is written into this image's tile in place.
            *self.tab.live.borrow_mut() = Some(image.clone());
        }
        let slot = if popup {
            &mut self.popup_image
        } else {
            &mut self.image
        };
        if let Some(old) = slot.replace(image) {
            cx.drop_image(old, None);
        }
    }

    fn apply(&mut self, update: Update, cx: &mut Context<Self>) {
        match update {
            Update::Created => {}
            Update::Closed => {
                self.tab.browser.borrow_mut().take();
                if !self.closed {
                    self.closed = true;
                    cx.emit(BrowserEvent::CloseRequested);
                }
            }
            Update::Title(title) => {
                self.title = title.into();
                cx.emit(BrowserEvent::TitleChanged(self.title()));
            }
            Update::Url(u) => {
                if u != self.url.as_ref() {
                    self.url = u.into();
                    let shown = self.url.clone();
                    // Whatever the user is typing wins over redirects and late commits.
                    self.address.update(cx, |f, cx| {
                        if !f.is_edited() {
                            f.set_text(address_text(&shown), cx)
                        }
                    });
                    cx.emit(BrowserEvent::UrlChanged(self.url.clone()));
                }
            }
            Update::Loading {
                loading,
                back,
                forward,
            } => {
                self.can_back = back;
                self.can_forward = forward;
                if loading != self.loading {
                    self.loading = loading;
                    if loading {
                        self.progress = 0.;
                    } else {
                        self.loads += 1;
                    }
                    cx.emit(BrowserEvent::LoadingChanged(loading));
                }
            }
            Update::Progress(p) => self.progress = p,
            Update::Cursor(c) => self.cursor = c,
            Update::Status(s) => self.status = (!s.is_empty()).then(|| s.into()),
            Update::OpenTab(u) => cx.emit(BrowserEvent::OpenInNewTab(u)),
            Update::Dialog(request) => self.open_dialog(request, cx),
            Update::DialogReset => self.dialog = None,
            Update::FileDialog(request) => self.open_file_dialog(request, cx),
            Update::Menu(m) => self.menu = Some(m),
            Update::Download(d) => {
                if d.done || d.failed {
                    let id = d.id;
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_secs(8)).await;
                        let _ = this.update(cx, |this, cx| {
                            if this.download.as_ref().is_some_and(|d| d.id == id) {
                                this.download = None;
                                cx.notify();
                            }
                        });
                    })
                    .detach();
                }
                self.download = Some(d);
            }
            Update::Favicon { bitmap, page } => {
                if let Some(b) = &bitmap {
                    super::favicons::remember(&page, &b.bgra, b.width, b.height, cx);
                }
                let image = bitmap.and_then(to_render_image);
                if let Some(old) = std::mem::replace(&mut self.favicon, image) {
                    cx.drop_image(old, None);
                }
                cx.emit(BrowserEvent::FaviconChanged);
            }
            Update::LoadStarted => {
                self.error = None;
                self.crashed = false;
            }
            Update::LoadError(e) => self.error = Some(e.into()),
            Update::Crashed(e) => {
                self.crashed = true;
                self.error = Some(e.into());
            }
            Update::Selection(s) => self.selection = s,
            // Browsers only let a page write the clipboard right after a user gesture.
            Update::PageCopy(text) => {
                if self
                    .last_input
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            Update::PopupShow(show) => {
                if !show {
                    self.popup_rect = None;
                    if let Some(old) = self.popup_image.take() {
                        cx.drop_image(old, None);
                    }
                }
            }
            Update::PopupRect(x, y, w, h) => {
                self.popup_rect = Some(Bounds::new(
                    point(px(x as f32), px(y as f32)),
                    size(px(w as f32), px(h as f32)),
                ));
            }
        }
    }

    fn open_dialog(&mut self, request: JsDialog, cx: &mut Context<Self>) {
        let (field, subscription) = if request.kind == DialogKind::Prompt {
            let field = cx.new(|cx| TextField::new("", cx));
            let text = request.default_text.clone();
            field.update(cx, |f, cx| {
                f.set_text(&text, cx);
                f.select_all(cx);
            });
            let sub = cx.subscribe(&field, |this, _, ev: &TextFieldEvent, cx| {
                this.finish_dialog(matches!(ev, TextFieldEvent::Submit), cx)
            });
            (Some(field), Some(sub))
        } else {
            (None, None)
        };
        self.menu = None;
        self.pending_focus = Some(match &field {
            Some(field) => field.focus_handle(cx),
            None => self.dialog_focus.clone(),
        });
        self.dialog = Some(Dialog {
            request,
            field,
            _subscription: subscription,
        });
    }

    /// Answers an `<input type=file>` (or `showSaveFilePicker`) with the system picker.
    /// CEF holds the page until `cont`/`cancel`, so every outcome must reach one of them.
    fn open_file_dialog(&mut self, request: FileDialogRequest, cx: &mut Context<Self>) {
        use std::path::PathBuf;
        let FileDialogRequest {
            kind,
            title,
            default_path,
            callback,
        } = request;
        let paths: std::pin::Pin<Box<dyn std::future::Future<Output = Vec<PathBuf>>>> =
            if kind == FileDialogKind::Save {
                let default = PathBuf::from(&default_path);
                let dir = default
                    .parent()
                    .filter(|p| p.is_absolute() && p.is_dir())
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| crate::platform::home().join("Downloads"));
                let name = default
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned());
                let rx = cx.prompt_for_new_path(&dir, name.as_deref());
                Box::pin(async move {
                    rx.await
                        .ok()
                        .and_then(|r| r.ok())
                        .flatten()
                        .into_iter()
                        .collect()
                })
            } else {
                let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
                    files: kind != FileDialogKind::Folder,
                    directories: kind == FileDialogKind::Folder,
                    multiple: kind == FileDialogKind::OpenMultiple,
                    prompt: (!title.is_empty()).then(|| title.into()),
                });
                Box::pin(async move {
                    rx.await
                        .ok()
                        .and_then(|r| r.ok())
                        .flatten()
                        .unwrap_or_default()
                })
            };
        cx.spawn(async move |_, _| {
            let picked = paths.await;
            if picked.is_empty() {
                callback.cancel();
                return;
            }
            let mut list = cef::CefStringList::new();
            for path in &picked {
                list.append(&path.to_string_lossy());
            }
            callback.cont(Some(&mut list));
        })
        .detach();
    }

    fn finish_dialog(&mut self, ok: bool, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        let input = dialog
            .field
            .as_ref()
            .map(|f| f.read(cx).text().to_string())
            .unwrap_or_default();
        dialog
            .request
            .callback
            .cont(ok as i32, Some(&input.as_str().into()));
        self.pending_focus = Some(self.page_focus.clone());
        cx.notify();
    }

    // --- view → CEF ------------------------------------------------------------------------

    fn address_event(
        &mut self,
        _: &Entity<TextField>,
        ev: &TextFieldEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            TextFieldEvent::Submit => {
                let text = self.address.read(cx).text().to_string();
                self.navigate(&text, cx);
                window.focus(&self.page_focus, cx);
            }
            TextFieldEvent::Cancel => {
                let shown = self.url.clone();
                self.address
                    .update(cx, |f, cx| f.set_text(address_text(&shown), cx));
                window.focus(&self.page_focus, cx);
            }
        }
    }

    fn home_field_event(
        &mut self,
        _: &Entity<TextField>,
        ev: &TextFieldEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            TextFieldEvent::Submit => {
                let text = self.home_field.read(cx).text().to_string();
                self.open_from_home(&text, window, cx);
            }
            TextFieldEvent::Cancel => self.home_field.update(cx, |f, cx| f.set_text("", cx)),
        }
    }

    /// A navigation from the new-tab page; an empty address stays there.
    fn open_from_home(&mut self, input: &str, window: &mut Window, cx: &mut Context<Self>) {
        if input.trim().is_empty() {
            return;
        }
        self.navigate(input, cx);
        window.focus(&self.page_focus, cx);
    }

    fn page_point(
        &self,
        position: Point<Pixels>,
        modifiers: &gpui::Modifiers,
        buttons: u32,
    ) -> cef::MouseEvent {
        let origin = self.page_bounds.get().origin;
        cef::MouseEvent {
            x: f32::from(position.x - origin.x).round() as i32,
            y: f32::from(position.y - origin.y).round() as i32,
            modifiers: keys::flags(modifiers) | buttons,
        }
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dialog) = &self.dialog {
            let focus = dialog
                .field
                .as_ref()
                .map_or_else(|| self.dialog_focus.clone(), |f| f.focus_handle(cx));
            window.focus(&focus, cx);
            return;
        }
        // The new-tab page handles its own clicks; the blank page under it gets none.
        if self.is_home() {
            return;
        }
        window.focus(&self.page_focus, cx);
        if self.menu.take().is_some() {
            cx.notify();
            return;
        }
        let Some(host) = self.tab.host() else { return };
        self.last_input = Some(Instant::now());
        host.set_focus(1);
        self.dragging = Some(ev.button);
        let e = self.page_point(ev.position, &ev.modifiers, 0);
        host.send_mouse_click_event(Some(&e), button_type(ev.button), 0, ev.click_count as i32);
    }

    fn mouse_up(&mut self, ev: &MouseUpEvent) {
        self.dragging = None;
        if self.is_home() {
            return;
        }
        let Some(host) = self.tab.host() else { return };
        let e = self.page_point(ev.position, &ev.modifiers, 0);
        host.send_mouse_click_event(Some(&e), button_type(ev.button), 1, ev.click_count as i32);
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, hovered: bool) {
        if self.is_home() {
            return;
        }
        let Some(host) = self.tab.host() else { return };
        if !hovered && self.dragging.is_none() {
            if std::mem::take(&mut self.hovered) {
                let e = self.page_point(ev.position, &ev.modifiers, 0);
                host.send_mouse_move_event(Some(&e), 1);
            }
            return;
        }
        self.hovered = hovered;
        let buttons = match ev.pressed_button {
            Some(MouseButton::Left) => keys::LEFT_BUTTON,
            Some(MouseButton::Middle) => keys::MIDDLE_BUTTON,
            Some(MouseButton::Right) => keys::RIGHT_BUTTON,
            _ => 0,
        };
        let e = self.page_point(ev.position, &ev.modifiers, buttons);
        host.send_mouse_move_event(Some(&e), 0);
    }

    fn scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
        if self.is_home() {
            return;
        }
        let Some(host) = self.tab.host() else { return };
        let (dx, dy) = match ev.delta {
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
            ScrollDelta::Lines(l) => (l.x * 50.0, l.y * 50.0),
        };
        let e = self.page_point(ev.position, &ev.modifiers, 0);
        host.send_mouse_wheel_event(Some(&e), dx.round() as i32, dy.round() as i32);
    }

    /// Browser shortcuts, handled before keys reach the page or the address field.
    fn shortcut(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        // Cmd on macOS does what Ctrl does elsewhere.
        let ctrl = if cfg!(target_os = "macos") {
            m.platform
        } else {
            m.control
        } && !m.alt;
        let handled = match ks.key.as_str() {
            "[" if cfg!(target_os = "macos") && ctrl => {
                self.back(cx);
                true
            }
            "]" if cfg!(target_os = "macos") && ctrl => {
                self.forward(cx);
                true
            }
            "l" if ctrl && !m.shift => self.focus_address_and(window, cx),
            "d" if m.alt && !m.control => self.focus_address_and(window, cx),
            "f6" => self.focus_address_and(window, cx),
            "r" if ctrl => {
                self.reload_inner(m.shift, cx);
                true
            }
            "f5" => {
                self.reload_inner(m.shift || m.control, cx);
                true
            }
            "left" if m.alt && !m.control && !m.shift => {
                self.back(cx);
                true
            }
            "right" if m.alt && !m.control && !m.shift => {
                self.forward(cx);
                true
            }
            "=" | "+" if ctrl => self.zoom(1, cx),
            "-" if ctrl => self.zoom(-1, cx),
            "0" if ctrl => self.zoom(0, cx),
            "f12" => {
                self.inspect(cx);
                true
            }
            "i" if ctrl && m.shift => {
                self.inspect(cx);
                true
            }
            "escape" if self.menu.is_some() => {
                self.menu = None;
                cx.notify();
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn focus_address_and(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.focus_address(window, cx);
        true
    }

    fn zoom(&mut self, direction: i32, _: &mut Context<Self>) -> bool {
        let Some(host) = self.tab.host() else {
            return true;
        };
        let factor = 1.2f64.powf(host.zoom_level());
        let next = match direction {
            0 => 1.0,
            d if d > 0 => ZOOM_FACTORS
                .iter()
                .copied()
                .find(|&z| z > factor + 0.01)
                .unwrap_or(2.0),
            _ => ZOOM_FACTORS
                .iter()
                .rev()
                .copied()
                .find(|&z| z < factor - 0.01)
                .unwrap_or(0.25),
        };
        host.set_zoom_level(next.ln() / 1.2f64.ln());
        true
    }

    fn key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // The page is modal while a JS dialog is up, wherever focus ended up.
        if self.dialog.is_some() {
            match ev.keystroke.key.as_str() {
                "enter" => self.finish_dialog(true, cx),
                "escape" => self.finish_dialog(false, cx),
                _ => {}
            }
            // Text keys must propagate: GPUI only feeds the prompt's input handler then.
            if ev.keystroke.key_char.is_none() || ev.keystroke.modifiers.control {
                cx.stop_propagation();
            }
            return;
        }
        if self.is_home() {
            return;
        }
        let Some(host) = self.tab.host() else { return };
        let ks: &Keystroke = &ev.keystroke;
        self.last_input = Some(Instant::now());
        // CEF can't reach the system clipboard on Linux (see `clipboard`), so arakun pastes for
        // it. Copy keys still go to the page, whose script reports the copy; the selection
        // written here first covers pages that run no script (PDFs, error pages).
        #[cfg(target_os = "linux")]
        {
            let m = &ks.modifiers;
            let paste =
                (m.control && ks.key == "v") || (m.shift && !m.control && ks.key == "insert");
            if paste && !m.alt && !m.platform {
                self.paste(cx);
                cx.stop_propagation();
                return;
            }
            let copy = m.control && !m.alt && matches!(ks.key.as_str(), "c" | "x" | "insert");
            if copy && !self.selection.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(self.selection.clone()));
            }
        }
        // On macOS the editing shortcuts come from the app's Edit menu, not from the page's
        // key handling, so they are run on the focused frame here.
        #[cfg(target_os = "macos")]
        {
            let m = &ks.modifiers;
            if m.platform && !m.control && !m.alt {
                let frame = self.tab.browser().and_then(|b| b.focused_frame());
                let done = match (ks.key.as_str(), m.shift, frame) {
                    ("c", false, Some(f)) => Some(f.copy()),
                    ("x", false, Some(f)) => Some(f.cut()),
                    ("v", false, Some(_)) => Some(self.paste(cx)),
                    ("a", false, Some(f)) => Some(f.select_all()),
                    ("z", false, Some(f)) => Some(f.undo()),
                    ("z", true, Some(f)) => Some(f.redo()),
                    _ => None,
                };
                if done.is_some() {
                    cx.stop_propagation();
                    return;
                }
            }
        }
        let mods = keys::flags(&ks.modifiers);
        let ch = keys::key_character(&ks.key, ks.key_char.as_deref());
        host.send_key_event(Some(&keys::event(
            KeyEventType::RAWKEYDOWN,
            &ks.key,
            mods,
            ch,
        )));
        // Printable text reaches the page through the input handler as CHAR events; Enter
        // and Tab never do, and forms only submit on Enter's CHAR.
        if ks.key == "enter" || ks.key == "tab" {
            host.send_key_event(Some(&keys::char_event(ch, mods)));
        }
        if ks.key == "escape" && self.loading {
            self.stop(cx);
        }
        if ks.key_char.is_none() || ks.modifiers.control {
            cx.stop_propagation();
        }
    }

    fn key_up(&mut self, ev: &KeyUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if self.is_home() {
            return;
        }
        let Some(host) = self.tab.host() else { return };
        let ks = &ev.keystroke;
        host.send_key_event(Some(&keys::event(
            KeyEventType::KEYUP,
            &ks.key,
            keys::flags(&ks.modifiers),
            0,
        )));
    }

    fn run_menu(&mut self, command: MenuCommand, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else { return };
        let frame = self.tab.browser().and_then(|b| b.focused_frame());
        match command {
            MenuCommand::Back => self.back(cx),
            MenuCommand::Forward => self.forward(cx),
            MenuCommand::Reload => self.reload(cx),
            MenuCommand::OpenLink => cx.emit(BrowserEvent::OpenInNewTab(menu.link)),
            MenuCommand::CopyLink => cx.write_to_clipboard(ClipboardItem::new_string(menu.link)),
            MenuCommand::OpenImage => {
                cx.emit(BrowserEvent::OpenInNewTab(menu.image.unwrap_or_default()))
            }
            MenuCommand::CopyImage => {
                cx.write_to_clipboard(ClipboardItem::new_string(menu.image.unwrap_or_default()))
            }
            MenuCommand::Copy => cx.write_to_clipboard(ClipboardItem::new_string(menu.selection)),
            MenuCommand::Cut => {
                cx.write_to_clipboard(ClipboardItem::new_string(menu.selection));
                if let Some(f) = frame {
                    f.del();
                }
            }
            MenuCommand::Paste => self.paste(cx),
            MenuCommand::SelectAll => {
                if let Some(f) = frame {
                    f.select_all();
                }
            }
            MenuCommand::Inspect => self.inspect(cx),
        }
        cx.notify();
    }

    /// Pastes the system clipboard's text into the page.
    fn paste(&mut self, cx: &mut Context<Self>) {
        let frame = self.tab.browser().and_then(|b| b.focused_frame());
        #[cfg(target_os = "linux")]
        {
            let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else {
                return;
            };
            if let Some(frame) = frame {
                let script = super::clipboard::paste_script(&text);
                frame.execute_java_script(Some(&script.as_str().into()), None, 0);
            } else if let Some(host) = self.tab.host() {
                host.ime_commit_text(Some(&text.as_str().into()), Some(&NO_RANGE), 0);
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = cx;
            if let Some(frame) = frame {
                frame.paste();
            }
        }
    }

    // --- rendering -------------------------------------------------------------------------

    fn render_home(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let open: home::Open = Rc::new(move |text: &str, window: &mut Window, cx: &mut App| {
            view.update(cx, |this, cx| this.open_from_home(text, window, cx));
        });
        let field = self.home_field.clone();
        let focus = field.focus_handle(cx);
        let mascot = self
            .home_mascot
            .get_or_insert_with(|| cx.new(|cx| Mascot::new(home::MASCOT_W, cx)))
            .clone();
        home::render(home::Home {
            info: self.home.clone(),
            size: self.page_bounds.get().size,
            field_focused: field.read(cx).is_focused(window),
            field,
            mascot,
            open,
            on_background: Box::new(move |window, cx| {
                if !focus.is_focused(window) {
                    window.focus(&focus, cx);
                }
            }),
        })
    }

    fn render_menu(&self, menu: &MenuRequest, cx: &mut Context<Self>) -> AnyElement {
        let mut items: Vec<(MenuCommand, &str, Option<&str>, bool)> = Vec::new();
        if !menu.link.is_empty() {
            items.push((MenuCommand::OpenLink, "Open link in new tab", None, true));
            items.push((MenuCommand::CopyLink, "Copy link address", None, true));
        }
        if menu.image.is_some() {
            items.push((MenuCommand::OpenImage, "Open image in new tab", None, true));
            items.push((MenuCommand::CopyImage, "Copy image address", None, true));
        }
        let has_selection = !menu.selection.is_empty();
        if menu.editable {
            items.push((MenuCommand::Cut, "Cut", Some("Ctrl+X"), has_selection));
            items.push((MenuCommand::Copy, "Copy", Some("Ctrl+C"), has_selection));
            items.push((MenuCommand::Paste, "Paste", Some("Ctrl+V"), true));
            items.push((MenuCommand::SelectAll, "Select all", Some("Ctrl+A"), true));
        } else if has_selection {
            items.push((MenuCommand::Copy, "Copy", Some("Ctrl+C"), true));
        }
        if items.is_empty() {
            items.push((MenuCommand::Back, "Back", Some("Alt+←"), self.can_back));
            items.push((
                MenuCommand::Forward,
                "Forward",
                Some("Alt+→"),
                self.can_forward,
            ));
            items.push((MenuCommand::Reload, "Reload", Some("Ctrl+R"), true));
        }
        let inspect = runtime::cdp_port().is_some();

        let page = self.page_bounds.get().size;
        let (w, h) = (
            210.,
            items.len() as f32 * 26. + if inspect { 26. + 9. } else { 0. } + 10.,
        );
        let x = (menu.x as f32).min(f32::from(page.width) - w - 4.).max(4.);
        let y = if menu.y as f32 + h > f32::from(page.height) - 4. {
            (menu.y as f32 - h).max(4.)
        } else {
            menu.y as f32
        };

        chrome::popover("browser-menu")
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(w))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(i, (cmd, label, shortcut, enabled))| {
                        let item =
                            chrome::menu_item(("browser-menu-item", i), label, shortcut, enabled);
                        if enabled {
                            item.on_click(cx.listener(move |this, _, _, cx| this.run_menu(cmd, cx)))
                        } else {
                            item
                        }
                    }),
            )
            .when(inspect, |d| {
                d.child(chrome::separator()).child(
                    chrome::menu_item("browser-menu-inspect", "Inspect", Some("F12"), true)
                        .on_click(
                            cx.listener(|this, _, _, cx| this.run_menu(MenuCommand::Inspect, cx)),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_dialog(&self, dialog: &Dialog, cx: &mut Context<Self>) -> AnyElement {
        let kind = dialog.request.kind;
        let host = self
            .url
            .split("://")
            .nth(1)
            .and_then(|r| r.split('/').next())
            .filter(|h| !h.is_empty())
            .map_or_else(|| "This page".to_string(), |h| h.to_string());
        div()
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_start()
            .pt(crate::ui::sp(24.))
            .pb(crate::ui::sp(12.))
            .px(crate::ui::sp(12.))
            .bg(gpui::hsla(0., 0., 0., 0.25))
            .child(
                chrome::popover("browser-dialog")
                    .track_focus(&self.dialog_focus)
                    .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                        match ev.keystroke.key.as_str() {
                            "enter" => this.finish_dialog(true, cx),
                            "escape" => this.finish_dialog(false, cx),
                            _ => return,
                        }
                        cx.stop_propagation();
                    }))
                    .w(px(440.))
                    .max_w_full()
                    .max_h_full()
                    .p(crate::ui::sp(16.))
                    .gap(crate::ui::sp(12.))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(crate::ui::TEXT_MD))
                            .child(format!("{host} says")),
                    )
                    .child(
                        div()
                            .id("browser-dialog-message")
                            .overflow_y_scroll()
                            .max_h(px(260.))
                            .min_h_0()
                            .text_size(px(crate::ui::TEXT_MD))
                            .text_color(theme::text_muted())
                            .child(dialog.request.message.clone()),
                    )
                    .children(dialog.field.clone().map(|field| {
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .h(px(28.))
                            .px(crate::ui::sp(8.))
                            .rounded(crate::ui::rad(8.))
                            .border_1()
                            .border_color(theme::border_strong())
                            .bg(theme::fill_hover())
                            .text_size(px(crate::ui::TEXT_MD))
                            .child(field)
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .justify_end()
                            .gap(crate::ui::sp(8.))
                            .when(kind != DialogKind::Alert, |d| {
                                d.child(
                                    chrome::button("browser-dialog-cancel", "Cancel", false)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.finish_dialog(false, cx)
                                        })),
                                )
                            })
                            .child(chrome::button("browser-dialog-ok", "OK", true).on_click(
                                cx.listener(|this, _, _, cx| this.finish_dialog(true, cx)),
                            )),
                    ),
            )
            .into_any_element()
    }

    fn render_download(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.download.as_ref()?;
        let label = if d.failed {
            format!("{} failed", d.name)
        } else if d.done {
            d.name.clone()
        } else if d.percent >= 0 {
            format!("{} {}%", d.name, d.percent)
        } else {
            format!("{}…", d.name)
        };
        let dir = std::path::Path::new(&d.path)
            .parent()
            .map(|p| p.to_path_buf());
        Some(
            div()
                .id("browser-download")
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(4.))
                .h(px(20.))
                .px(crate::ui::sp(6.))
                .max_w(px(220.))
                .overflow_hidden()
                .rounded(crate::ui::rad(4.))
                .bg(theme::fill_selected())
                .text_size(px(crate::ui::TEXT_XS))
                .text_color(if d.failed {
                    gpui::Hsla::from(theme::danger())
                } else {
                    theme::mix(0.7)
                })
                .cursor_pointer()
                .hover(|s| s.bg(theme::fill_selected()))
                .child(
                    icon(
                        if d.done {
                            "checkmark-circle02"
                        } else {
                            "circle-arrow-down01"
                        },
                        12.,
                    )
                    .text_color(theme::text_subtle()),
                )
                .child(div().truncate().child(label))
                .on_click(cx.listener(move |_, _, _, _| {
                    if let Some(dir) = &dir {
                        crate::platform::open(dir);
                    }
                }))
                .into_any_element(),
        )
    }
}

fn to_render_image(b: Bitmap) -> Option<Arc<RenderImage>> {
    let buf = image::RgbaImage::from_raw(b.width, b.height, b.bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])))
}

fn button_type(b: MouseButton) -> cef::MouseButtonType {
    match b {
        MouseButton::Right => cef::MouseButtonType::RIGHT,
        MouseButton::Middle => cef::MouseButtonType::MIDDLE,
        _ => cef::MouseButtonType::LEFT,
    }
}

/// Where to draw a page bitmap of `pixels` meant to cover `logical`: on whole
/// screen pixels and one texel per pixel when it was rendered at `scale`. A pane
/// that starts at a fractional pixel (a split, a UI zoom of 110 %) would
/// otherwise resample the page and blur it. A bitmap at another density (CEF
/// ignoring the scale) is stretched over `logical` as before.
fn pixel_aligned(
    origin: Point<Pixels>,
    pixels: gpui::Size<gpui::DevicePixels>,
    logical: gpui::Size<Pixels>,
    scale: f32,
) -> Bounds<Pixels> {
    let snap = |v: Pixels| px((f32::from(v) * scale).round() / scale);
    let native = size(
        px(pixels.width.0 as f32 / scale),
        px(pixels.height.0 as f32 / scale),
    );
    let close = (native.width - logical.width).abs() <= px(2.)
        && (native.height - logical.height).abs() <= px(2.);
    Bounds::new(
        point(snap(origin.x), snap(origin.y)),
        if close { native } else { logical },
    )
}

/// Tells CEF about a new page size or scale factor.
fn resize(tab: &TabState, bounds: Bounds<Pixels>, scale: f32) {
    let size = (
        f32::from(bounds.size.width).round() as i32,
        f32::from(bounds.size.height).round() as i32,
    );
    if size.0 <= 0 || size.1 <= 0 {
        return;
    }
    let size_changed = tab.view_size.replace(size) != size;
    let scale_changed = tab.scale.replace(scale) != scale;
    if let Some(host) = tab.host() {
        if scale_changed {
            host.notify_screen_info_changed();
        }
        if size_changed || scale_changed {
            host.was_resized();
        }
    }
}

impl EntityInputHandler for BrowserView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        if std::mem::take(&mut self.composing)
            && let Some(host) = self.tab.host()
        {
            host.ime_finish_composing_text(0);
        }
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let Some(host) = self.tab.host() else { return };
        if std::mem::take(&mut self.composing) {
            host.ime_commit_text(Some(&text.into()), Some(&NO_RANGE), 0);
            return;
        }
        for unit in text.encode_utf16().filter(|&u| u >= 0x20) {
            host.send_key_event(Some(&keys::char_event(unit, 0)));
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let Some(host) = self.tab.host() else { return };
        self.composing = !text.is_empty();
        if text.is_empty() {
            host.ime_cancel_composition();
        } else {
            let end = text.encode_utf16().count() as u32;
            let caret = cef::Range { from: end, to: end };
            host.ime_set_composition(Some(&text.into()), None, Some(&NO_RANGE), Some(&caret));
        }
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(focus) = self.pending_focus.take() {
            window.defer(cx, move |window, cx| window.focus(&focus, cx));
        }

        let address_focused = self.address.read(cx).is_focused(window);
        let toolbar = chrome::toolbar(
            Toolbar {
                can_back: self.can_back,
                can_forward: self.can_forward,
                can_reload: true,
                loading: self.loading && self.error.is_none(),
                progress: self.progress,
                address_focused,
                favicon: self.favicon.clone(),
                download: self.render_download(cx),
                on_back: Box::new(cx.listener(|this, _, _, cx| this.back(cx))),
                on_forward: Box::new(cx.listener(|this, _, _, cx| this.forward(cx))),
                on_reload: Box::new(cx.listener(|this, _, _, cx| {
                    if this.loading {
                        this.stop(cx)
                    } else {
                        this.reload(cx)
                    }
                })),
            },
            self.address.clone(),
        );

        let entity = cx.entity();
        let tab = self.tab.clone();
        let paint_tab = self.tab.clone();
        let home = self.is_home();
        let image = self.image.clone().filter(|_| !self.closed && !home);
        let image_view = self.image_view;
        let popup = self
            .popup_image
            .clone()
            .zip(self.popup_rect)
            .filter(|_| !home);
        let page_bounds = self.page_bounds.clone();
        let focus = self.page_focus.clone();
        let cursor = if home {
            CursorStyle::Arrow
        } else {
            self.cursor
        };
        let dragging = self.dragging.is_some();
        if !home {
            self.home_mascot = None;
        }
        let home_page = home.then(|| self.render_home(window, cx));

        let page = div()
            .id("browser-page")
            .relative()
            .flex_1()
            .overflow_hidden()
            .bg(if image.is_some() {
                self.page_color.into()
            } else {
                theme::background()
            })
            .track_focus(&self.page_focus)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .child(
                canvas(
                    move |bounds, window, _| {
                        resize(&tab, bounds, window.scale_factor());
                        window.insert_hitbox(bounds, HitboxBehavior::Normal)
                    },
                    move |bounds, hitbox, window, cx| {
                        // The new-tab page lays itself out from the size painted last; a
                        // frame that changed it draws once more.
                        let last = page_bounds.replace(bounds);
                        if home && last.size != bounds.size {
                            let view = entity.clone();
                            window.defer(cx, move |_, cx| view.update(cx, |_, cx| cx.notify()));
                        }
                        window.set_cursor_style(cursor, &hitbox);
                        if paint_tab.atlas.borrow().is_none() {
                            *paint_tab.atlas.borrow_mut() = Some(window.sprite_atlas());
                        }
                        let scale = window.scale_factor();
                        if let Some(image) = image {
                            let s = image.size(0);
                            let logical = if image_view.0 > 0 && image_view.1 > 0 {
                                size(px(image_view.0 as f32), px(image_view.1 as f32))
                            } else {
                                size(px(s.width.0 as f32 / scale), px(s.height.0 as f32 / scale))
                            };
                            let image_bounds = pixel_aligned(bounds.origin, s, logical, scale);
                            let _ = window.paint_image(
                                bounds,
                                image_bounds,
                                Corners::default(),
                                image,
                                0,
                                false,
                            );
                        }
                        if let Some((image, rect)) = popup {
                            let s = image.size(0);
                            // The popup's rect is in logical units; draw the bitmap over it.
                            let logical = if rect.size.width > px(0.) && rect.size.height > px(0.) {
                                rect.size
                            } else {
                                size(px(s.width.0 as f32 / scale), px(s.height.0 as f32 / scale))
                            };
                            let image_bounds = Bounds::new(bounds.origin + rect.origin, logical);
                            let _ = window.paint_image(
                                bounds,
                                image_bounds,
                                Corners::default(),
                                image,
                                0,
                                false,
                            );
                        }
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, entity.clone()),
                            cx,
                        );

                        let view = entity.clone();
                        let hit = hitbox.clone();
                        window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                            if phase == DispatchPhase::Bubble {
                                let hovered = hit.is_hovered(window);
                                view.update(cx, |this, _| this.mouse_move(ev, hovered));
                            }
                        });
                        let view = entity.clone();
                        window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, cx| {
                            if phase == DispatchPhase::Bubble
                                && (dragging || hitbox.is_hovered(window))
                            {
                                view.update(cx, |this, _| this.mouse_up(ev));
                            }
                        });
                    },
                )
                .size_full(),
            )
            .children(home_page)
            .when_some(
                self.status.clone().filter(|_| self.menu.is_none()),
                |d, status| {
                    d.child(
                        div()
                            .absolute()
                            .left_0()
                            .bottom_0()
                            .max_w(px(560.))
                            .h(px(22.))
                            .px(crate::ui::sp(8.))
                            .flex()
                            .items_center()
                            .rounded_tr(crate::ui::rad(6.))
                            .bg(chrome::panel_bg())
                            .border_t_1()
                            .border_r_1()
                            .border_color(theme::border())
                            .text_size(px(crate::ui::TEXT_SM))
                            .text_color(theme::text_muted())
                            .child(div().truncate().child(status)),
                    )
                },
            )
            .when_some(self.error.clone(), |d, error| {
                let (title, icon_name) = if self.crashed {
                    ("This page crashed", "alert-circle")
                } else if self.tab.browser.borrow().is_none() && !self.closed {
                    ("The browser is not available", "globe")
                } else {
                    ("This page could not be loaded", "alert-circle")
                };
                let retry = self.tab.browser.borrow().is_some().then(|| {
                    chrome::button("browser-retry", "Try again", false)
                        .on_click(cx.listener(|this, _, _, cx| this.reload(cx)))
                        .into_any_element()
                });
                d.child(chrome::notice(icon_name, title, Some(error), retry))
            })
            .when_some(
                self.menu.as_ref().map(|m| self.render_menu(m, cx)),
                |d, menu| d.child(menu),
            )
            .when_some(
                self.dialog.as_ref().map(|dlg| self.render_dialog(dlg, cx)),
                |d, dialog| d.child(dialog),
            );

        div()
            .id("browser-view")
            .key_context("Browser")
            .flex()
            .flex_col()
            .size_full()
            .capture_key_down(cx.listener(Self::shortcut))
            .child(toolbar)
            .child(page)
    }
}

/// A blank page shows an empty address field (with its placeholder), as in Chrome.
fn address_text(url: &str) -> &str {
    if url == "about:blank" { "" } else { url }
}

impl Drop for BrowserView {
    fn drop(&mut self) {
        if let Some(host) = self.tab.host() {
            host.close_browser(1);
        }
        self.tab.browser.borrow_mut().take();
    }
}
