//! CEF handlers. Per-tab callbacks run on the main thread inside `do_message_loop_work`
//! (or synchronously inside a `BrowserHost` call); they only record what happened in the
//! tab's [`TabState`], which the view drains after the pump. Nothing here touches GPUI
//! entities, so re-entrancy is harmless.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use cef::*;
use futures::channel::oneshot;
use serde_json::Value;

use super::ConsoleLine;
use super::runtime::{self, PumpSignal};

// The `cef` crate binds the native cursor of `on_cursor_change` as a byte pointer on macOS.
#[cfg(target_os = "macos")]
type CursorHandle = *mut u8;
#[cfg(not(target_os = "macos"))]
type CursorHandle = cef::sys::cef_cursor_handle_t;

pub struct Bitmap {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    Alert,
    Confirm,
    Prompt,
}

pub struct JsDialog {
    pub kind: DialogKind,
    pub message: String,
    pub default_text: String,
    pub callback: JsdialogCallback,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileDialogKind {
    Open,
    OpenMultiple,
    Folder,
    Save,
}

pub struct FileDialogRequest {
    pub kind: FileDialogKind,
    pub title: String,
    pub default_path: String,
    pub callback: FileDialogCallback,
}

pub struct MenuRequest {
    pub x: i32,
    pub y: i32,
    pub link: String,
    pub selection: String,
    pub editable: bool,
    pub image: Option<String>,
}

#[derive(Clone)]
pub struct DownloadStatus {
    pub id: u32,
    pub name: String,
    pub path: String,
    pub percent: i32,
    pub done: bool,
    pub failed: bool,
}

pub enum Update {
    Created,
    Closed,
    Title(String),
    Url(String),
    Loading {
        loading: bool,
        back: bool,
        forward: bool,
    },
    Progress(f64),
    Cursor(gpui::CursorStyle),
    Status(String),
    OpenTab(String),
    Dialog(JsDialog),
    DialogReset,
    FileDialog(FileDialogRequest),
    Menu(MenuRequest),
    Download(DownloadStatus),
    /// The page's icon, or `None` when it has none that loads. `page` is the address
    /// it was found on, which the icon cache files it under.
    Favicon {
        bitmap: Option<Bitmap>,
        page: String,
    },
    LoadStarted,
    LoadError(String),
    Crashed(String),
    Selection(String),
    /// Text the page copied (Linux only, see `clipboard`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    PageCopy(String),
    PopupShow(bool),
    PopupRect(i32, i32, i32, i32),
}

/// The answer to a DevTools Protocol command: its `result`, or the error's message.
pub type DevToolsResult = Result<Value, String>;

/// Messages kept for `arakun page console`; older ones are dropped.
const MAX_CONSOLE: usize = 1000;

pub struct TabState {
    /// Logical (DIP) size CEF lays the page out at.
    pub view_size: Cell<(i32, i32)>,
    /// The view size when CEF painted the last main frame: the logical size that frame
    /// covers, whatever device scale CEF actually rendered it at.
    pub frame_view: Cell<(i32, i32)>,
    pub scale: Cell<f32>,
    pub browser: RefCell<Option<Browser>>,
    pub frame: RefCell<Option<Bitmap>>,
    pub popup_frame: RefCell<Option<Bitmap>>,
    pub updates: RefCell<Vec<Update>>,
    /// Set by any callback; the pump syncs the view when it sees it.
    pub dirty: Cell<bool>,
    pub paints: Cell<u64>,
    pub visible: Cell<bool>,
    /// The window's sprite atlas, once the view has painted. A frame the size of the one
    /// on screen is written into that image's tile where it changed, instead of copied
    /// whole into a new image every time.
    pub atlas: RefCell<Option<Arc<dyn gpui::PlatformAtlas>>>,
    /// The page image on screen, which frames update in place.
    pub live: RefCell<Option<Arc<gpui::RenderImage>>>,
    /// `live` was replaced from here (a GPU frame of a new size); the view adopts it.
    pub live_changed: Cell<bool>,
    /// A frame was written into `live` in place; the view repaints.
    pub updated: Cell<bool>,
    /// The page colour along the right and bottom edges of the last frame written in place.
    pub edge: Cell<Option<[u8; 3]>>,
    /// What the page logged since its main frame last started loading.
    pub console: RefCell<VecDeque<ConsoleLine>>,
    console_seq: Cell<u64>,
    /// DevTools commands waiting for their answer, by message id.
    calls: RefCell<HashMap<i32, oneshot::Sender<DevToolsResult>>>,
    last_call: Cell<i32>,
    /// The icon URLs still to try for the page's icon, after the one downloading.
    favicon_queue: RefCell<VecDeque<String>>,
    /// The page those icons are for.
    favicon_page: RefCell<String>,
    /// Counts the page's icon lists: a download for an older list is ignored.
    favicon_round: Cell<u64>,
}

pub type Tab = Rc<TabState>;

impl TabState {
    pub fn new(width: i32, height: i32, scale: f32) -> Tab {
        Rc::new(TabState {
            view_size: Cell::new((width, height)),
            frame_view: Cell::new((width, height)),
            scale: Cell::new(scale),
            browser: RefCell::new(None),
            frame: RefCell::new(None),
            popup_frame: RefCell::new(None),
            updates: RefCell::new(Vec::new()),
            dirty: Cell::new(true),
            paints: Cell::new(0),
            visible: Cell::new(true),
            atlas: RefCell::new(None),
            live: RefCell::new(None),
            live_changed: Cell::new(false),
            updated: Cell::new(false),
            edge: Cell::new(None),
            console: RefCell::new(VecDeque::new()),
            console_seq: Cell::new(0),
            calls: RefCell::new(HashMap::new()),
            last_call: Cell::new(0),
            favicon_queue: RefCell::new(VecDeque::new()),
            favicon_page: RefCell::new(String::new()),
            favicon_round: Cell::new(0),
        })
    }

    /// Writes the dirty rects of a CPU frame into the tile of the image on screen. False
    /// when there is no such tile of this size yet, and the frame must go the long way.
    fn update_in_place(&self, buffer: *const u8, width: i32, height: i32, dirty: &[Rect]) -> bool {
        if buffer.is_null() || width <= 0 || height <= 0 {
            return false;
        }
        let Some(atlas) = self.atlas.borrow().clone() else {
            return false;
        };
        let Some(image) = self.live.borrow().clone() else {
            return false;
        };
        let s = image.size(0);
        if (s.width.0, s.height.0) != (width, height) {
            return false;
        }
        let key = gpui::AtlasKey::from(gpui::RenderImageParams {
            image_id: image.id,
            frame_index: 0,
        });
        let stride = width as usize * 4;
        let pixels = unsafe { std::slice::from_raw_parts(buffer, stride * height as usize) };
        for r in dirty {
            let x = r.x.clamp(0, width);
            let y = r.y.clamp(0, height);
            let w = (r.x + r.width).clamp(0, width) - x;
            let h = (r.y + r.height).clamp(0, height) - y;
            if w == 0 || h == 0 {
                continue;
            }
            let region = gpui::Bounds::new(
                gpui::point(gpui::DevicePixels(x), gpui::DevicePixels(y)),
                gpui::size(gpui::DevicePixels(w), gpui::DevicePixels(h)),
            );
            let start = y as usize * stride + x as usize * 4;
            if !atlas.update_tile(&key, region, &pixels[start..], stride) {
                return false;
            }
        }
        self.edge
            .set(edge_color(pixels, width as usize, height as usize));
        true
    }

    /// Copies a GPU frame (a DMA-BUF) into the tile of the image on screen, making that
    /// image first when the size changed. False when it could not, so nothing changed.
    #[cfg(target_os = "linux")]
    fn copy_gpu_frame(&self, info: &AcceleratedPaintInfo) -> bool {
        let Some(atlas) = self.atlas.borrow().clone() else {
            return false;
        };
        let (width, height) = (
            info.extra.visible_rect.width,
            info.extra.visible_rect.height,
        );
        if width <= 0 || height <= 0 || info.plane_count <= 0 {
            return false;
        }
        let fourcc = if info.format == ColorType::RGBA_8888 {
            u32::from_le_bytes(*b"AB24")
        } else {
            u32::from_le_bytes(*b"AR24")
        };
        let frame = gpui::DmaBufFrame {
            width: info.extra.coded_size.width.max(width) as u32,
            height: info.extra.coded_size.height.max(height) as u32,
            fourcc,
            modifier: info.modifier,
            planes: info.planes[..info.plane_count.min(4) as usize]
                .iter()
                .map(|p| gpui::DmaBufPlane {
                    fd: p.fd,
                    offset: p.offset,
                    stride: p.stride,
                })
                .collect(),
        };
        if runtime::timing() && self.paints.get().is_multiple_of(600) {
            eprintln!(
                "arakun-browser: gpu frame {}x{} coded {}x{} planes {} modifier {:#x} format {:?}",
                width,
                height,
                frame.width,
                frame.height,
                info.plane_count,
                info.modifier,
                info.format
            );
        }
        let current = self.live.borrow().clone().filter(|image| {
            let s = image.size(0);
            (s.width.0, s.height.0) == (width, height)
        });
        let image = match current {
            Some(image) => image,
            None => {
                let blank = image::RgbaImage::new(width as u32, height as u32);
                let image = Arc::new(gpui::RenderImage::new(vec![image::Frame::new(blank)]));
                let key = gpui::AtlasKey::from(gpui::RenderImageParams {
                    image_id: image.id,
                    frame_index: 0,
                });
                let bytes = image.as_bytes(0).unwrap_or_default();
                let made = atlas.get_or_insert_with(key, &mut || {
                    Ok(Some((image.size(0), std::borrow::Cow::Borrowed(bytes))))
                });
                if !matches!(made, Ok(Some(_))) {
                    return false;
                }
                *self.live.borrow_mut() = Some(image.clone());
                self.live_changed.set(true);
                image
            }
        };
        let key = gpui::AtlasKey::from(gpui::RenderImageParams {
            image_id: image.id,
            frame_index: 0,
        });
        atlas.copy_dmabuf_into_tile(&key, &frame)
    }

    pub fn push(&self, update: Update) {
        self.updates.borrow_mut().push(update);
        self.dirty.set(true);
    }

    pub fn browser(&self) -> Option<Browser> {
        self.browser.borrow().clone()
    }

    pub fn host(&self) -> Option<BrowserHost> {
        self.browser.borrow().as_ref().and_then(|b| b.host())
    }

    /// Starts trying the icons of a new list, dropping what was left of the last one.
    fn start_favicons(page: &Page, host: &BrowserHost, icons: Vec<String>, page_url: String) {
        let tab = &page.tab;
        tab.favicon_round.set(tab.favicon_round.get() + 1);
        *tab.favicon_queue.borrow_mut() = super::favicons::candidates(&icons, &page_url).into();
        *tab.favicon_page.borrow_mut() = page_url;
        Self::next_favicon(page, host);
    }

    /// Downloads the next icon to try, or says the page has none once every one failed.
    fn next_favicon(page: &Page, host: &BrowserHost) {
        let tab = &page.tab;
        // No borrow may be held here: the download can answer before it returns.
        let next = tab.favicon_queue.borrow_mut().pop_front();
        let Some(url) = next else {
            let page_url = tab.favicon_page.borrow().clone();
            tab.push(Update::Favicon {
                bitmap: None,
                page: page_url,
            });
            return;
        };
        let size = (16.0 * tab.scale.get()).ceil() as u32;
        let mut callback = FaviconCallbackBuilder::new(page.clone(), tab.favicon_round.get());
        host.download_image(Some(&url.as_str().into()), 1, size, 0, Some(&mut callback));
    }

    fn log_console(&self, level: LogSeverity, text: String, source: String, line: i32) {
        let level = match level {
            l if l == LogSeverity::ERROR || l == LogSeverity::FATAL => "error",
            l if l == LogSeverity::WARNING => "warn",
            l if l == LogSeverity::INFO => "log",
            _ => "debug",
        };
        let seq = self.console_seq.get() + 1;
        self.console_seq.set(seq);
        let source = if line > 0 && !source.is_empty() {
            format!("{source}:{line}")
        } else {
            source
        };
        let mut console = self.console.borrow_mut();
        if console.len() >= MAX_CONSOLE {
            console.pop_front();
        }
        console.push_back(ConsoleLine {
            seq,
            level,
            text,
            source,
        });
    }

    /// Sends a DevTools Protocol command to the page. The answer comes back through
    /// the observer from [`dev_tools_observer`], which must be registered first.
    pub fn call(&self, method: &str, params: Value) -> oneshot::Receiver<DevToolsResult> {
        let (tx, rx) = oneshot::channel();
        let Some(host) = self.host() else {
            let _ = tx.send(Err("the page is not open".into()));
            return rx;
        };
        let id = self.last_call.get() + 1;
        self.last_call.set(id);
        let message =
            serde_json::json!({ "id": id, "method": method, "params": params }).to_string();
        self.calls.borrow_mut().insert(id, tx);
        if host.send_dev_tools_message(Some(message.as_bytes())) == 0
            && let Some(tx) = self.calls.borrow_mut().remove(&id)
        {
            let _ = tx.send(Err(format!("Chromium refused {method}")));
        }
        rx
    }
}

fn s(value: Option<&CefString>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

fn userfree(value: CefStringUserfree) -> String {
    CefString::from(&value).to_string()
}

/// The colour along a frame's right and bottom edges, where a wider or taller pane shows
/// past it. Points a scrollbar's width in from the edge; the most common wins.
pub(super) fn edge_color(bgra: &[u8], w: usize, h: usize) -> Option<[u8; 3]> {
    if w == 0 || h == 0 || bgra.len() < w * h * 4 {
        return None;
    }
    let x = w.saturating_sub(24).max(w / 2);
    let y = h.saturating_sub(24).max(h / 2);
    let points = [
        (x, h / 4),
        (x, h / 2),
        (x, h * 3 / 4),
        (w / 4, y),
        (w / 2, y),
        (w * 3 / 4, y),
    ];
    let pixels: Vec<[u8; 3]> = points
        .iter()
        .map(|&(x, y)| {
            let i = (y * w + x) * 4;
            [bgra[i + 2], bgra[i + 1], bgra[i]]
        })
        .collect();
    pixels
        .iter()
        .max_by_key(|p| pixels.iter().filter(|q| q == p).count())
        .copied()
}

fn copy_bitmap(buffer: *const u8, width: i32, height: i32) -> Option<Bitmap> {
    if buffer.is_null() || width <= 0 || height <= 0 {
        return None;
    }
    let len = width as usize * height as usize * 4;
    let bgra = unsafe { std::slice::from_raw_parts(buffer, len) }.to_vec();
    Some(Bitmap {
        bgra,
        width: width as u32,
        height: height as u32,
    })
}

// --- App / browser process -----------------------------------------------------------------

/// The CDP port, baked into `--remote-allow-origins` so a DevTools frontend tab served by
/// Chromium itself may connect back.
#[derive(Clone)]
pub struct AppState {
    pub pump: Option<Arc<PumpSignal>>,
    pub cdp_port: u16,
}

wrap_app! {
    pub struct AppBuilder {
        state: AppState,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else { return };
            if process_type.is_some_and(|t| !t.to_string().is_empty()) {
                return;
            }
            for switch in ["no-startup-window", "noerrdialogs", "hide-crash-restore-bubble"] {
                command_line.append_switch(Some(&switch.into()));
            }
            // Shared frames come with an explicit layout (a DRM modifier) only from ANGLE on
            // EGL; on its default backend Chromium leaves the modifier out.
            if gpu_frames() {
                command_line.append_switch_with_value(Some(&"use-angle".into()), Some(&"gl-egl".into()));
            }
            // No keyring prompt for a profile that only arakun uses.
            command_line.append_switch_with_value(Some(&"password-store".into()), Some(&"basic".into()));
            // On Wayland Chromium takes the scale from the output and ignores the one
            // `screen_info` gives, so pages render at 1x and the window stretches them
            // blurry under a fractional or UI-zoomed scale. Over X11 (XWayland) it honors
            // it; the browser draws off screen and never opens a window there either way.
            // Shared GPU frames need Wayland: over X11 Chromium hands them over without a
            // DRM modifier, and the window cannot tell how their memory is laid out.
            #[cfg(target_os = "linux")]
            if std::env::var_os("DISPLAY").is_some() && !gpu_frames() {
                command_line.append_switch_with_value(Some(&"ozone-platform".into()), Some(&"x11".into()));
            }
            if self.state.cdp_port != 0 {
                let origin = format!("http://127.0.0.1:{}", self.state.cdp_port);
                command_line.append_switch_with_value(
                    Some(&"remote-allow-origins".into()),
                    Some(&origin.as_str().into()),
                );
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            let pump = self.state.pump.clone()?;
            Some(ProcessHandlerBuilder::new(pump))
        }
    }
}

pub fn app(state: AppState) -> App {
    AppBuilder::new(state)
}

wrap_browser_process_handler! {
    pub struct ProcessHandlerBuilder {
        pump: Arc<PumpSignal>,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            runtime::CONTEXT_READY.store(true, Ordering::SeqCst);
            self.pump.schedule(0);
        }

        // Called from any thread.
        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            self.pump.schedule(delay_ms);
        }

        // Another process started CEF on our profile. Claiming it handled stops
        // Chromium from opening a window of its own here.
        fn on_already_running_app_relaunch(
            &self,
            _command_line: Option<&mut CommandLine>,
            _current_directory: Option<&CefString>,
        ) -> ::std::os::raw::c_int {
            1
        }
    }
}

// --- Per-tab handlers ----------------------------------------------------------------------

#[derive(Clone)]
pub struct Page {
    tab: Tab,
}

wrap_render_handler! {
    pub struct RenderHandlerBuilder {
        page: Page,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                let (w, h) = self.page.tab.view_size.get();
                *rect = Rect { x: 0, y: 0, width: w.max(1), height: h.max(1) };
            }
        }

        fn screen_info(&self, _browser: Option<&mut Browser>, info: Option<&mut ScreenInfo>) -> ::std::os::raw::c_int {
            let Some(info) = info else { return 0 };
            let (w, h) = self.page.tab.view_size.get();
            info.device_scale_factor = self.page.tab.scale.get();
            info.depth = 24;
            info.depth_per_component = 8;
            // Popups (<select> lists) are kept inside the page area we can draw.
            info.rect = Rect { x: 0, y: 0, width: w.max(1), height: h.max(1) };
            info.available_rect = info.rect.clone();
            1
        }

        fn on_popup_show(&self, _browser: Option<&mut Browser>, show: ::std::os::raw::c_int) {
            if show == 0 {
                self.page.tab.popup_frame.borrow_mut().take();
            }
            self.page.tab.push(Update::PopupShow(show != 0));
        }

        fn on_popup_size(&self, _browser: Option<&mut Browser>, rect: Option<&Rect>) {
            if let Some(r) = rect {
                self.page.tab.push(Update::PopupRect(r.x, r.y, r.width, r.height));
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            let tab = &self.page.tab;
            if type_ == PaintElementType::VIEW
                && let Some(dirty) = dirty_rects
                && tab.update_in_place(buffer, width, height, dirty)
            {
                if runtime::timing() && tab.paints.get().is_multiple_of(600) {
                    let area: i64 = dirty.iter().map(|r| r.width as i64 * r.height as i64).sum();
                    eprintln!(
                        "arakun-browser: frame {} in place, {} rects, {:.0}% of {width}x{height}",
                        tab.paints.get(),
                        dirty.len(),
                        area as f64 * 100. / (width as f64 * height as f64)
                    );
                }
                tab.frame_view.set(tab.view_size.get());
                tab.paints.set(tab.paints.get() + 1);
                tab.updated.set(true);
                tab.dirty.set(true);
                return;
            }
            if runtime::timing() && type_ == PaintElementType::VIEW {
                eprintln!("arakun-browser: frame {} copied whole ({width}x{height})", tab.paints.get());
            }
            let Some(bitmap) = copy_bitmap(buffer, width, height) else { return };
            if type_ == PaintElementType::POPUP {
                *tab.popup_frame.borrow_mut() = Some(bitmap);
            } else {
                *tab.frame.borrow_mut() = Some(bitmap);
                tab.frame_view.set(tab.view_size.get());
                tab.paints.set(tab.paints.get() + 1);
            }
            tab.dirty.set(true);
        }

        #[cfg(target_os = "linux")]
        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            // Popups (a <select>'s list) still come as CPU frames; only the page is shared.
            let tab = &self.page.tab;
            let Some(info) = info else { return };
            if type_ != PaintElementType::VIEW || !tab.copy_gpu_frame(info) {
                return;
            }
            tab.frame_view.set(tab.view_size.get());
            tab.paints.set(tab.paints.get() + 1);
            tab.updated.set(true);
            tab.dirty.set(true);
        }

        fn on_text_selection_changed(
            &self,
            _browser: Option<&mut Browser>,
            selected_text: Option<&CefString>,
            _selected_range: Option<&cef::Range>,
        ) {
            self.page.tab.push(Update::Selection(s(selected_text)));
        }
    }
}

wrap_display_handler! {
    pub struct DisplayHandlerBuilder {
        page: Page,
    }

    impl DisplayHandler {
        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            self.page.tab.push(Update::Title(s(title)));
        }

        fn on_address_change(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            if frame.is_some_and(|f| f.is_main() != 0) {
                self.page.tab.push(Update::Url(s(url)));
            }
        }

        fn on_status_message(&self, _browser: Option<&mut Browser>, value: Option<&CefString>) {
            self.page.tab.push(Update::Status(s(value)));
        }

        fn on_loading_progress_change(&self, _browser: Option<&mut Browser>, progress: f64) {
            self.page.tab.push(Update::Progress(progress));
        }

        fn on_console_message(
            &self,
            _browser: Option<&mut Browser>,
            level: LogSeverity,
            message: Option<&CefString>,
            source: Option<&CefString>,
            line: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let message = s(message);
            #[cfg(target_os = "linux")]
            if let Some(text) = message.strip_prefix(super::clipboard::COPY_TAG) {
                self.page.tab.push(Update::PageCopy(text.to_string()));
                return 1;
            }
            self.page.tab.log_console(level, message, s(source), line);
            0
        }

        fn on_favicon_urlchange(&self, browser: Option<&mut Browser>, icon_urls: Option<&mut CefStringList>) {
            let icons: Vec<String> = icon_urls
                .map(|list| list.clone().into_iter().map(|u| u.to_string()).collect())
                .unwrap_or_default();
            let Some(browser) = browser else { return };
            let page_url = browser
                .main_frame()
                .map(|f| userfree(f.url()))
                .unwrap_or_default();
            let Some(host) = browser.host() else { return };
            TabState::start_favicons(&self.page, &host, icons, page_url);
        }

        fn on_cursor_change(
            &self,
            _browser: Option<&mut Browser>,
            _cursor: CursorHandle,
            type_: CursorType,
            _custom_cursor_info: Option<&CursorInfo>,
        ) -> ::std::os::raw::c_int {
            self.page.tab.push(Update::Cursor(cursor_style(type_)));
            1
        }
    }
}

fn cursor_style(t: CursorType) -> gpui::CursorStyle {
    use gpui::CursorStyle as C;
    match t {
        t if t == CursorType::HAND => C::PointingHand,
        t if t == CursorType::IBEAM => C::IBeam,
        t if t == CursorType::VERTICALTEXT => C::IBeamCursorForVerticalLayout,
        t if t == CursorType::CROSS || t == CursorType::CELL => C::Crosshair,
        t if t == CursorType::GRAB => C::OpenHand,
        t if t == CursorType::GRABBING || t == CursorType::MOVE => C::ClosedHand,
        t if t == CursorType::EASTRESIZE => C::ResizeRight,
        t if t == CursorType::WESTRESIZE => C::ResizeLeft,
        t if t == CursorType::NORTHRESIZE => C::ResizeUp,
        t if t == CursorType::SOUTHRESIZE => C::ResizeDown,
        t if t == CursorType::EASTWESTRESIZE => C::ResizeLeftRight,
        t if t == CursorType::NORTHSOUTHRESIZE => C::ResizeUpDown,
        t if t == CursorType::COLUMNRESIZE => C::ResizeColumn,
        t if t == CursorType::ROWRESIZE => C::ResizeRow,
        t if t == CursorType::NORTHEASTSOUTHWESTRESIZE
            || t == CursorType::NORTHEASTRESIZE
            || t == CursorType::SOUTHWESTRESIZE =>
        {
            C::ResizeUpRightDownLeft
        }
        t if t == CursorType::NORTHWESTSOUTHEASTRESIZE
            || t == CursorType::NORTHWESTRESIZE
            || t == CursorType::SOUTHEASTRESIZE =>
        {
            C::ResizeUpLeftDownRight
        }
        t if t == CursorType::NOTALLOWED || t == CursorType::NODROP => C::OperationNotAllowed,
        t if t == CursorType::CONTEXTMENU => C::ContextualMenu,
        t if t == CursorType::ALIAS => C::DragLink,
        t if t == CursorType::COPY => C::DragCopy,
        _ => C::Arrow,
    }
}

wrap_download_image_callback! {
    pub struct FaviconCallbackBuilder {
        page: Page,
        round: u64,
    }

    impl DownloadImageCallback {
        fn on_download_image_finished(
            &self,
            _image_url: Option<&CefString>,
            _http_status_code: ::std::os::raw::c_int,
            image: Option<&mut Image>,
        ) {
            let bitmap = image.and_then(|image| {
                let (mut w, mut h) = (0, 0);
                let data = image.as_bitmap(
                    self.page.tab.scale.get(),
                    ColorType::BGRA_8888,
                    AlphaType::PREMULTIPLIED,
                    Some(&mut w),
                    Some(&mut h),
                )?;
                let len = data.size();
                let mut bgra = vec![0u8; len];
                data.data(Some(&mut bgra), 0);
                (w > 0 && h > 0 && len == w as usize * h as usize * 4)
                    .then_some(Bitmap { bgra, width: w as u32, height: h as u32 })
            });
            let tab = &self.page.tab;
            if tab.favicon_round.get() != self.round {
                return;
            }
            if bitmap.is_some() {
                tab.favicon_queue.borrow_mut().clear();
                let page = tab.favicon_page.borrow().clone();
                tab.push(Update::Favicon { bitmap, page });
                return;
            }
            // This one did not draw (an SVG, a 404): try the next.
            if let Some(host) = tab.host() {
                TabState::next_favicon(&self.page, &host);
            }
        }
    }
}

wrap_life_span_handler! {
    pub struct LifeSpanHandlerBuilder {
        page: Page,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            // Every popup becomes a arakun tab, never a native window. Like Chrome's popup
            // blocker, scripts may only open one in response to user input.
            let explicit_tab = target_disposition == WindowOpenDisposition::NEW_FOREGROUND_TAB
                || target_disposition == WindowOpenDisposition::NEW_BACKGROUND_TAB;
            let url = s(target_url);
            if (user_gesture != 0 || explicit_tab) && !url.is_empty() {
                self.page.tab.push(Update::OpenTab(url));
            }
            1
        }

        fn on_after_created(&self, _browser: Option<&mut Browser>) {
            self.page.tab.push(Update::Created);
        }

        fn do_close(&self, _browser: Option<&mut Browser>) -> ::std::os::raw::c_int {
            0
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.page.tab.push(Update::Closed);
        }
    }
}

wrap_load_handler! {
    pub struct LoadHandlerBuilder {
        page: Page,
    }

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            can_go_back: ::std::os::raw::c_int,
            can_go_forward: ::std::os::raw::c_int,
        ) {
            self.page.tab.push(Update::Loading {
                loading: is_loading != 0,
                back: can_go_back != 0,
                forward: can_go_forward != 0,
            });
        }

        fn on_load_start(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _transition_type: TransitionType) {
            #[cfg(target_os = "linux")]
            if let Some(f) = frame.as_deref() {
                watch_clipboard(f);
            }
            if frame.is_some_and(|f| f.is_main() != 0) {
                // Like Chrome's console without "Preserve log".
                self.page.tab.console.borrow_mut().clear();
                self.page.tab.push(Update::LoadStarted);
            }
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            if error_code == Errorcode::ABORTED || !frame.is_some_and(|f| f.is_main() != 0) {
                return;
            }
            let url = s(failed_url);
            self.page.tab.push(Update::LoadError(format!("{} ({})", s(error_text), url)));
        }

        fn on_load_end(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _http_status_code: ::std::os::raw::c_int) {
            // Again once loaded: at load start the script can still land in the old document.
            #[cfg(target_os = "linux")]
            if let Some(f) = frame.as_deref() {
                watch_clipboard(f);
            }
            #[cfg(not(target_os = "linux"))]
            let _ = frame;
        }
    }
}

#[cfg(target_os = "linux")]
fn watch_clipboard(frame: &Frame) {
    let script = super::clipboard::watch_script();
    frame.execute_java_script(Some(&script.as_str().into()), None, 0);
}

wrap_jsdialog_handler! {
    pub struct JsDialogHandlerBuilder {
        page: Page,
    }

    impl JsdialogHandler {
        fn on_jsdialog(
            &self,
            _browser: Option<&mut Browser>,
            _origin_url: Option<&CefString>,
            dialog_type: JsdialogType,
            message_text: Option<&CefString>,
            default_prompt_text: Option<&CefString>,
            callback: Option<&mut JsdialogCallback>,
            _suppress_message: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            let Some(callback) = callback else { return 0 };
            let kind = if dialog_type == JsdialogType::CONFIRM {
                DialogKind::Confirm
            } else if dialog_type == JsdialogType::PROMPT {
                DialogKind::Prompt
            } else {
                DialogKind::Alert
            };
            self.page.tab.push(Update::Dialog(JsDialog {
                kind,
                message: s(message_text),
                default_text: s(default_prompt_text),
                callback: callback.clone(),
            }));
            1
        }

        fn on_before_unload_dialog(
            &self,
            _browser: Option<&mut Browser>,
            _message_text: Option<&CefString>,
            _is_reload: ::std::os::raw::c_int,
            callback: Option<&mut JsdialogCallback>,
        ) -> ::std::os::raw::c_int {
            // "Leave site?" always leaves: closing or navigating a tab is explicit in arakun.
            let Some(callback) = callback else { return 0 };
            callback.cont(1, None);
            1
        }

        fn on_reset_dialog_state(&self, _browser: Option<&mut Browser>) {
            self.page.tab.push(Update::DialogReset);
        }
    }
}

wrap_dialog_handler! {
    pub struct DialogHandlerBuilder {
        page: Page,
    }

    impl DialogHandler {
        // GPUI's portal picker takes no type filters, so `accept` lists are ignored.
        fn on_file_dialog(
            &self,
            _browser: Option<&mut Browser>,
            mode: FileDialogMode,
            title: Option<&CefString>,
            default_file_path: Option<&CefString>,
            _accept_filters: Option<&mut CefStringList>,
            _accept_extensions: Option<&mut CefStringList>,
            _accept_descriptions: Option<&mut CefStringList>,
            callback: Option<&mut FileDialogCallback>,
        ) -> ::std::os::raw::c_int {
            let Some(callback) = callback else { return 0 };
            let kind = if mode == FileDialogMode::OPEN_MULTIPLE {
                FileDialogKind::OpenMultiple
            } else if mode == FileDialogMode::OPEN_FOLDER {
                FileDialogKind::Folder
            } else if mode == FileDialogMode::SAVE {
                FileDialogKind::Save
            } else {
                FileDialogKind::Open
            };
            self.page.tab.push(Update::FileDialog(FileDialogRequest {
                kind,
                title: s(title),
                default_path: s(default_file_path),
                callback: callback.clone(),
            }));
            1
        }
    }
}

wrap_context_menu_handler! {
    pub struct ContextMenuHandlerBuilder {
        page: Page,
    }

    impl ContextMenuHandler {
        fn on_before_context_menu(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            params: Option<&mut ContextMenuParams>,
            model: Option<&mut MenuModel>,
        ) {
            // The native menu is replaced by a GPUI one; an empty model shows nothing.
            if let Some(model) = model {
                model.clear();
            }
            let Some(p) = params else { return };
            let image = (p.media_type() == ContextMenuMediaType::IMAGE)
                .then(|| userfree(p.source_url()))
                .filter(|u| !u.is_empty());
            self.page.tab.push(Update::Menu(MenuRequest {
                x: p.xcoord(),
                y: p.ycoord(),
                link: userfree(p.link_url()),
                selection: userfree(p.selection_text()),
                editable: p.is_editable() != 0,
                image,
            }));
        }
    }
}

wrap_download_handler! {
    pub struct DownloadHandlerBuilder {
        page: Page,
    }

    impl DownloadHandler {
        fn can_download(
            &self,
            _browser: Option<&mut Browser>,
            _url: Option<&CefString>,
            _request_method: Option<&CefString>,
        ) -> ::std::os::raw::c_int {
            1
        }

        fn on_before_download(
            &self,
            _browser: Option<&mut Browser>,
            _download_item: Option<&mut DownloadItem>,
            suggested_name: Option<&CefString>,
            callback: Option<&mut BeforeDownloadCallback>,
        ) -> ::std::os::raw::c_int {
            let Some(callback) = callback else { return 0 };
            let path = download_path(&s(suggested_name));
            callback.cont(Some(&path.to_string_lossy().as_ref().into()), 0);
            1
        }

        fn on_download_updated(
            &self,
            _browser: Option<&mut Browser>,
            item: Option<&mut DownloadItem>,
            _callback: Option<&mut DownloadItemCallback>,
        ) {
            let Some(item) = item else { return };
            let path = userfree(item.full_path());
            if path.is_empty() {
                return;
            }
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.page.tab.push(Update::Download(DownloadStatus {
                id: item.id(),
                name,
                path,
                percent: item.percent_complete(),
                done: item.is_complete() != 0,
                failed: item.is_canceled() != 0 || item.is_interrupted() != 0,
            }));
        }
    }
}

/// `~/Downloads/<name>`, adding " (n)" before the extension while the name is taken.
fn download_path(suggested: &str) -> PathBuf {
    let home = crate::platform::home();
    let dir = home.join("Downloads");
    let _ = std::fs::create_dir_all(&dir);
    let name = std::path::Path::new(suggested)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "download".into());
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    let mut path = dir.join(&name);
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    path
}

wrap_request_handler! {
    pub struct RequestHandlerBuilder {
        page: Page,
    }

    impl RequestHandler {
        // Middle-click and Ctrl+click on links.
        fn on_open_urlfrom_tab(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            target_url: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            if target_disposition == WindowOpenDisposition::CURRENT_TAB {
                return 0;
            }
            self.page.tab.push(Update::OpenTab(s(target_url)));
            1
        }

        fn on_render_process_terminated(
            &self,
            _browser: Option<&mut Browser>,
            _status: TerminationStatus,
            error_code: ::std::os::raw::c_int,
            error_string: Option<&CefString>,
        ) {
            self.page.tab.push(Update::Crashed(format!("{} (code {error_code})", s(error_string))));
        }
    }
}

wrap_dev_tools_message_observer! {
    pub struct DevToolsObserverBuilder {
        page: Page,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_message(&self, _browser: Option<&mut Browser>, message: Option<&[u8]>) -> ::std::os::raw::c_int {
            let Some(message) = message.and_then(|m| serde_json::from_slice::<Value>(m).ok()) else { return 0 };
            let Some(id) = message.get("id").and_then(Value::as_i64) else { return 0 };
            let Some(reply) = self.page.tab.calls.borrow_mut().remove(&(id as i32)) else { return 0 };
            let _ = reply.send(devtools_result(message));
            1
        }
    }
}

fn devtools_result(mut message: Value) -> DevToolsResult {
    let Some(error) = message.get("error") else {
        return Ok(message
            .get_mut("result")
            .map(Value::take)
            .unwrap_or(Value::Null));
    };
    let text = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("DevTools error");
    Err(match error.get("data").and_then(Value::as_str) {
        Some(data) => format!("{text}: {data}"),
        None => text.to_string(),
    })
}

pub fn dev_tools_observer(tab: &Tab) -> DevToolsMessageObserver {
    DevToolsObserverBuilder::new(Page { tab: tab.clone() })
}

wrap_client! {
    pub struct ClientBuilder {
        render: RenderHandler,
        display: DisplayHandler,
        life_span: LifeSpanHandler,
        load: LoadHandler,
        jsdialog: JsdialogHandler,
        context_menu: ContextMenuHandler,
        download: DownloadHandler,
        request: RequestHandler,
        dialog: DialogHandler,
    }

    impl Client {
        fn dialog_handler(&self) -> Option<DialogHandler> {
            Some(self.dialog.clone())
        }
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }
        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }
        fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
            Some(self.jsdialog.clone())
        }
        fn context_menu_handler(&self) -> Option<ContextMenuHandler> {
            Some(self.context_menu.clone())
        }
        fn download_handler(&self) -> Option<DownloadHandler> {
            Some(self.download.clone())
        }
        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.request.clone())
        }
    }
}

/// Frames per second CEF renders a page at: the display's refresh rate, which CEF no
/// longer caps at 60. `ARAKUN_BROWSER_FPS` overrides it.
pub fn frame_rate() -> i32 {
    static FPS: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
    *FPS.get_or_init(|| {
        std::env::var("ARAKUN_BROWSER_FPS")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
            .or_else(crate::platform::refresh_rate)
            .unwrap_or(60)
            .clamp(1, 360)
    })
}

/// Whether pages hand their frames over on the GPU (a DMA-BUF the window copies into its
/// atlas) rather than as pixels in memory. Off unless `ARAKUN_BROWSER_GPU=1`: it works on
/// RADV, but measured no faster than CPU frames updated in place (a pan repaints the whole
/// page either way, and the copy waits on a GPU the page keeps busy), and it needs Chromium
/// on Wayland, which ignores a fractional UI scale. With it CEF sends no CPU frames for the
/// page, so a frame the window cannot import shows nothing.
pub fn gpu_frames() -> bool {
    cfg!(target_os = "linux") && std::env::var("ARAKUN_BROWSER_GPU").is_ok_and(|v| v == "1")
}

pub fn create_browser(url: &str, tab: &Tab) -> Option<Browser> {
    let window_info = WindowInfo {
        windowless_rendering_enabled: 1,
        shared_texture_enabled: gpu_frames() as i32,
        ..Default::default()
    };
    let settings = BrowserSettings {
        windowless_frame_rate: frame_rate(),
        background_color: 0xFFFF_FFFF,
        ..Default::default()
    };
    let page = Page { tab: tab.clone() };
    let mut client = ClientBuilder::new(
        RenderHandlerBuilder::new(page.clone()),
        DisplayHandlerBuilder::new(page.clone()),
        LifeSpanHandlerBuilder::new(page.clone()),
        LoadHandlerBuilder::new(page.clone()),
        JsDialogHandlerBuilder::new(page.clone()),
        ContextMenuHandlerBuilder::new(page.clone()),
        DownloadHandlerBuilder::new(page.clone()),
        RequestHandlerBuilder::new(page.clone()),
        DialogHandlerBuilder::new(page),
    );
    browser_host_create_browser_sync(
        Some(&window_info),
        Some(&mut client),
        Some(&CefString::from(url)),
        Some(&settings),
        None,
        None,
    )
}
