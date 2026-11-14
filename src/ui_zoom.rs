//! Whole-window UI zoom, like a browser's Ctrl+= / Ctrl+-.
//!
//! The window gpui draws into is wrapped so it reports a scale factor `zoom`
//! times the display's and a size `zoom` times smaller, and every position
//! that crosses the boundary (mouse events in, IME and window-menu positions
//! out) is converted. gpui then lays out and rasterizes the whole UI, borders
//! and glyphs included, at the zoomed density, and the app's own pixel math
//! stays in one coordinate space: nothing in the views knows about the zoom.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Result;
use futures::channel::oneshot;
use gpui::{
    A11yCallbacks, Action, ActivityGuard, AnyWindowHandle, AppLifecyclePhase, BackgroundExecutor,
    Bounds, Capslock, ClipboardItem, ClipboardReadError, CursorStyle, Decorations,
    DispatchEventResult, ExternalDragPayload, FileDropEvent, ForegroundExecutor, GpuSpecs, Keymap,
    Menu, MenuItem, Modifiers, OwnedMenu, PathPromptOptions, Pixels, Platform, PlatformAtlas,
    PlatformDisplay, PlatformGestures, PlatformInput, PlatformInputHandler, PlatformKeyboardLayout,
    PlatformKeyboardMapper, PlatformTextSystem, PlatformWindow, Point, PromptButton, PromptLevel,
    RequestFrameOptions, ResizeEdge, Scene, ScreenCaptureSource, ScrollDelta, Size,
    SystemNotification, SystemNotificationResponse, SystemWindowTab, Task, TextInputConfiguration,
    TextInputStateChange, ThermalState, WindowAppearance, WindowBackgroundAppearance, WindowBounds,
    WindowButtonLayout, WindowControlArea, WindowControls, WindowDecorations, WindowInsets,
    WindowParams, WindowVisibility, point, size,
};
use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle};
use smallvec::SmallVec;

/// UI zoom, in percent of the base size.
pub const MIN: u32 = 50;
pub const MAX: u32 = 200;
pub const STEP: i32 = 10;
static ZOOM: AtomicU32 = AtomicU32::new(100);

pub fn pct() -> u32 {
    ZOOM.load(Ordering::Relaxed)
}

fn factor() -> f32 {
    pct() as f32 / 100.
}

/// Takes effect on the next `Window::bounds_changed`, which re-reads the
/// wrapped scale factor and size.
pub fn set_pct(pct: u32) {
    ZOOM.store(pct.clamp(MIN, MAX), Ordering::Relaxed);
}

/// `pct` moved by `steps` of `STEP`, clamped to the range.
pub fn next(pct: u32, steps: i32) -> u32 {
    (pct as i32 + steps * STEP).clamp(MIN as i32, MAX as i32) as u32
}

/// The OS platform, with every window it opens wrapped in a [`ZoomWindow`].
pub fn platform() -> Rc<dyn Platform> {
    Rc::new(ZoomPlatform(gpui_platform::current_platform(false)))
}

fn scale_point(p: Point<Pixels>, k: f32) -> Point<Pixels> {
    point(p.x * k, p.y * k)
}

fn scale_size(s: Size<Pixels>, k: f32) -> Size<Pixels> {
    size(s.width * k, s.height * k)
}

fn scale_bounds(b: Bounds<Pixels>, k: f32) -> Bounds<Pixels> {
    Bounds::new(scale_point(b.origin, k), scale_size(b.size, k))
}

/// Converts an event's window positions by `k` (1 / zoom on the way in).
fn scale_input(input: PlatformInput, k: f32) -> PlatformInput {
    use PlatformInput as I;
    let p = |at: Point<Pixels>| scale_point(at, k);
    match input {
        I::MouseDown(mut e) => {
            e.position = p(e.position);
            I::MouseDown(e)
        }
        I::MouseUp(mut e) => {
            e.position = p(e.position);
            I::MouseUp(e)
        }
        I::MousePressure(mut e) => {
            e.position = p(e.position);
            I::MousePressure(e)
        }
        I::MouseMove(mut e) => {
            e.position = p(e.position);
            I::MouseMove(e)
        }
        I::MouseExited(mut e) => {
            e.position = p(e.position);
            I::MouseExited(e)
        }
        I::ScrollWheel(mut e) => {
            e.position = p(e.position);
            if let ScrollDelta::Pixels(d) = &mut e.delta {
                *d = p(*d);
            }
            I::ScrollWheel(e)
        }
        I::Pinch(mut e) => {
            e.position = p(e.position);
            I::Pinch(e)
        }
        I::LongPress(mut e) => {
            e.position = p(e.position);
            e.start_position = p(e.start_position);
            I::LongPress(e)
        }
        I::TouchDrag(mut e) => {
            e.position = p(e.position);
            e.start_position = p(e.start_position);
            I::TouchDrag(e)
        }
        I::Touch(mut e) => {
            e.position = p(e.position);
            e.predicted_position = e.predicted_position.map(p);
            I::Touch(e)
        }
        I::FileDrop(e) => I::FileDrop(match e {
            FileDropEvent::Entered { position, paths } => {
                FileDropEvent::Entered { position: p(position), paths }
            }
            FileDropEvent::Pending { position } => FileDropEvent::Pending { position: p(position) },
            FileDropEvent::Submit { position } => FileDropEvent::Submit { position: p(position) },
            other => other,
        }),
        other @ (I::KeyDown(_) | I::KeyUp(_) | I::ModifiersChanged(_)) => other,
    }
}

struct ZoomPlatform(Rc<dyn Platform>);

impl Platform for ZoomPlatform {
    fn open_window(&self, handle: AnyWindowHandle, options: WindowParams) -> Result<Box<dyn PlatformWindow>> {
        // The island (a layer surface) keeps its size whatever the window's zoom.
        #[cfg(target_os = "linux")]
        if matches!(options.kind, gpui::WindowKind::LayerShell(_)) {
            return self.0.open_window(handle, options);
        }
        let inner = self.0.open_window(handle, options)?;
        Ok(Box::new(ZoomWindow { inner }))
    }

    fn background_executor(&self) -> BackgroundExecutor {
        self.0.background_executor()
    }
    fn foreground_executor(&self) -> ForegroundExecutor {
        self.0.foreground_executor()
    }
    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.0.text_system()
    }
    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        self.0.run(on_finish_launching)
    }
    fn quit(&self) {
        self.0.quit()
    }
    fn restart(&self, binary_path: Option<PathBuf>, arguments: Vec<OsString>) {
        self.0.restart(binary_path, arguments)
    }
    fn activate(&self, ignoring_other_apps: bool) {
        self.0.activate(ignoring_other_apps)
    }
    fn hide(&self) {
        self.0.hide()
    }
    fn hide_other_apps(&self) {
        self.0.hide_other_apps()
    }
    fn unhide_other_apps(&self) {
        self.0.unhide_other_apps()
    }
    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        self.0.displays()
    }
    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        self.0.primary_display()
    }
    fn active_window(&self) -> Option<AnyWindowHandle> {
        self.0.active_window()
    }
    fn window_stack(&self) -> Option<Vec<AnyWindowHandle>> {
        self.0.window_stack()
    }
    fn is_screen_capture_supported(&self) -> bool {
        self.0.is_screen_capture_supported()
    }
    fn screen_capture_sources(&self) -> oneshot::Receiver<Result<Vec<Rc<dyn ScreenCaptureSource>>>> {
        self.0.screen_capture_sources()
    }
    fn window_appearance(&self) -> WindowAppearance {
        self.0.window_appearance()
    }
    fn set_window_appearance(&self, appearance: Option<WindowAppearance>) {
        self.0.set_window_appearance(appearance)
    }
    fn button_layout(&self) -> Option<WindowButtonLayout> {
        self.0.button_layout()
    }
    fn open_url(&self, url: &str) {
        self.0.open_url(url)
    }
    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) {
        self.0.on_open_urls(callback)
    }
    fn register_url_scheme(&self, url: &str) -> Task<Result<()>> {
        self.0.register_url_scheme(url)
    }
    fn prompt_for_paths(&self, options: PathPromptOptions) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        self.0.prompt_for_paths(options)
    }
    fn prompt_for_new_path(
        &self,
        directory: &Path,
        suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        self.0.prompt_for_new_path(directory, suggested_name)
    }
    fn can_select_mixed_files_and_dirs(&self) -> bool {
        self.0.can_select_mixed_files_and_dirs()
    }
    fn reveal_path(&self, path: &Path) {
        self.0.reveal_path(path)
    }
    fn open_with_system(&self, path: &Path) {
        self.0.open_with_system(path)
    }
    fn on_quit(&self, callback: Box<dyn FnMut() -> bool>) {
        self.0.on_quit(callback)
    }
    fn on_reopen(&self, callback: Box<dyn FnMut()>) {
        self.0.on_reopen(callback)
    }
    fn on_system_sleep(&self, callback: Box<dyn FnMut()>) {
        self.0.on_system_sleep(callback)
    }
    fn on_system_wake(&self, callback: Box<dyn FnMut()>) {
        self.0.on_system_wake(callback)
    }
    fn on_app_lifecycle(&self, callback: Box<dyn FnMut(AppLifecyclePhase)>) {
        self.0.on_app_lifecycle(callback)
    }
    fn on_memory_warning(&self, callback: Box<dyn FnMut()>) {
        self.0.on_memory_warning(callback)
    }
    fn gestures(&self) -> Option<Rc<dyn PlatformGestures>> {
        self.0.gestures()
    }
    fn set_menus(&self, menus: Vec<Menu>, keymap: &Keymap) {
        self.0.set_menus(menus, keymap)
    }
    fn get_menus(&self) -> Option<Vec<OwnedMenu>> {
        self.0.get_menus()
    }
    fn set_dock_menu(&self, menu: Vec<MenuItem>, keymap: &Keymap) {
        self.0.set_dock_menu(menu, keymap)
    }
    fn perform_dock_menu_action(&self, action: usize) {
        self.0.perform_dock_menu_action(action)
    }
    fn add_recent_document(&self, path: &Path) {
        self.0.add_recent_document(path)
    }
    fn update_jump_list(
        &self,
        menus: Vec<MenuItem>,
        entries: Vec<SmallVec<[PathBuf; 2]>>,
    ) -> Task<Vec<SmallVec<[PathBuf; 2]>>> {
        self.0.update_jump_list(menus, entries)
    }
    fn on_app_menu_action(&self, callback: Box<dyn FnMut(&dyn Action)>) {
        self.0.on_app_menu_action(callback)
    }
    fn on_will_open_app_menu(&self, callback: Box<dyn FnMut()>) {
        self.0.on_will_open_app_menu(callback)
    }
    fn on_validate_app_menu_command(&self, callback: Box<dyn FnMut(&dyn Action) -> bool>) {
        self.0.on_validate_app_menu_command(callback)
    }
    fn thermal_state(&self) -> ThermalState {
        self.0.thermal_state()
    }
    fn on_thermal_state_change(&self, callback: Box<dyn FnMut()>) {
        self.0.on_thermal_state_change(callback)
    }
    fn prevent_idle_sleep(&self, reason: &str) -> Task<Result<ActivityGuard>> {
        self.0.prevent_idle_sleep(reason)
    }
    fn set_app_identity(&self, identifier: &str, name: &str) {
        self.0.set_app_identity(identifier, name)
    }
    fn show_system_notification(&self, notification: SystemNotification) {
        self.0.show_system_notification(notification)
    }
    fn dismiss_system_notification(&self, tag: &str) {
        self.0.dismiss_system_notification(tag)
    }
    fn on_system_notification_response(&self, callback: Box<dyn FnMut(SystemNotificationResponse)>) {
        self.0.on_system_notification_response(callback)
    }
    fn compositor_name(&self) -> &'static str {
        self.0.compositor_name()
    }
    fn app_path(&self) -> Result<PathBuf> {
        self.0.app_path()
    }
    fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf> {
        self.0.path_for_auxiliary_executable(name)
    }
    fn set_cursor_style(&self, style: CursorStyle) {
        self.0.set_cursor_style(style)
    }
    fn hide_cursor_until_mouse_moves(&self) {
        self.0.hide_cursor_until_mouse_moves()
    }
    fn is_cursor_visible(&self) -> bool {
        self.0.is_cursor_visible()
    }
    fn should_auto_hide_scrollbars(&self) -> bool {
        self.0.should_auto_hide_scrollbars()
    }
    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        self.0.read_from_clipboard()
    }
    fn write_to_clipboard(&self, item: ClipboardItem) {
        self.0.write_to_clipboard(item)
    }
    fn read_from_clipboard_async(&self) -> Task<Result<Option<ClipboardItem>, ClipboardReadError>> {
        self.0.read_from_clipboard_async()
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn read_from_primary(&self) -> Option<ClipboardItem> {
        self.0.read_from_primary()
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn write_to_primary(&self, item: ClipboardItem) {
        self.0.write_to_primary(item)
    }
    #[cfg(target_os = "macos")]
    fn read_from_find_pasteboard(&self) -> Option<ClipboardItem> {
        self.0.read_from_find_pasteboard()
    }
    #[cfg(target_os = "macos")]
    fn write_to_find_pasteboard(&self, item: ClipboardItem) {
        self.0.write_to_find_pasteboard(item)
    }
    fn write_credentials(&self, url: &str, username: &str, password: &[u8]) -> Task<Result<()>> {
        self.0.write_credentials(url, username, password)
    }
    fn read_credentials(&self, url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        self.0.read_credentials(url)
    }
    fn delete_credentials(&self, url: &str) -> Task<Result<()>> {
        self.0.delete_credentials(url)
    }
    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        self.0.keyboard_layout()
    }
    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        self.0.keyboard_mapper()
    }
    fn on_keyboard_layout_change(&self, callback: Box<dyn FnMut()>) {
        self.0.on_keyboard_layout_change(callback)
    }
}

/// gpui's view of the OS window: `zoom` times denser and `zoom` times smaller.
struct ZoomWindow {
    inner: Box<dyn PlatformWindow>,
}

impl HasWindowHandle for ZoomWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        self.inner.window_handle()
    }
}

impl HasDisplayHandle for ZoomWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.inner.display_handle()
    }
}

impl PlatformWindow for ZoomWindow {
    fn content_size(&self) -> Size<Pixels> {
        scale_size(self.inner.content_size(), 1. / factor())
    }
    fn visual_viewport_bounds(&self) -> Bounds<Pixels> {
        scale_bounds(self.inner.visual_viewport_bounds(), 1. / factor())
    }
    fn resize(&mut self, size: Size<Pixels>) {
        self.inner.resize(scale_size(size, factor()))
    }
    fn scale_factor(&self) -> f32 {
        self.inner.scale_factor() * factor()
    }
    fn mouse_position(&self) -> Point<Pixels> {
        scale_point(self.inner.mouse_position(), 1. / factor())
    }
    fn on_input(&self, mut callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.inner.on_input(Box::new(move |input| callback(scale_input(input, 1. / factor()))))
    }
    fn on_resize(&self, mut callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.inner.on_resize(Box::new(move |size, scale| {
            let z = factor();
            callback(scale_size(size, 1. / z), scale * z)
        }))
    }
    fn update_ime_position(&self, bounds: Bounds<Pixels>) {
        self.inner.update_ime_position(scale_bounds(bounds, factor()))
    }
    fn show_window_menu(&self, position: Point<Pixels>) {
        self.inner.show_window_menu(scale_point(position, factor()))
    }
    fn set_client_inset(&self, inset: Pixels) {
        self.inner.set_client_inset(inset * factor())
    }
    fn set_exclusive_zone(&self, zone: Pixels) {
        self.inner.set_exclusive_zone(zone * factor())
    }
    fn set_input_region(&self, region: Option<&[Bounds<Pixels>]>) {
        let z = factor();
        let scaled: Option<Vec<_>> = region.map(|r| r.iter().map(|b| scale_bounds(*b, z)).collect());
        self.inner.set_input_region(scaled.as_deref())
    }
    #[cfg(target_os = "macos")]
    fn set_traffic_light_position(&self, position: Point<Pixels>) {
        self.inner.set_traffic_light_position(position)
    }

    // Screen-space geometry and everything else pass straight through.
    fn bounds(&self) -> Bounds<Pixels> {
        self.inner.bounds()
    }
    fn is_maximized(&self) -> bool {
        self.inner.is_maximized()
    }
    fn window_bounds(&self) -> WindowBounds {
        self.inner.window_bounds()
    }
    fn inner_window_bounds(&self) -> WindowBounds {
        self.inner.inner_window_bounds()
    }
    fn on_visual_viewport_changed(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_visual_viewport_changed(callback)
    }
    fn prepare_frame(&self) -> bool {
        self.inner.prepare_frame()
    }
    fn appearance(&self) -> WindowAppearance {
        self.inner.appearance()
    }
    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        self.inner.display()
    }
    fn modifiers(&self) -> Modifiers {
        self.inner.modifiers()
    }
    fn capslock(&self) -> Capslock {
        self.inner.capslock()
    }
    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.inner.set_input_handler(input_handler)
    }
    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.inner.take_input_handler()
    }
    fn set_text_input_configuration(&mut self, configuration: TextInputConfiguration) {
        self.inner.set_text_input_configuration(configuration)
    }
    fn prompt(
        &self,
        level: PromptLevel,
        msg: &str,
        detail: Option<&str>,
        answers: &[PromptButton],
    ) -> Option<oneshot::Receiver<usize>> {
        self.inner.prompt(level, msg, detail, answers)
    }
    fn activate(&self) {
        self.inner.activate()
    }
    fn request_attention(&self) {
        self.inner.request_attention()
    }
    fn is_active(&self) -> bool {
        self.inner.is_active()
    }
    fn visibility(&self) -> WindowVisibility {
        self.inner.visibility()
    }
    fn is_hovered(&self) -> bool {
        self.inner.is_hovered()
    }
    fn background_appearance(&self) -> WindowBackgroundAppearance {
        self.inner.background_appearance()
    }
    fn set_title(&mut self, title: &str) {
        self.inner.set_title(title)
    }
    fn set_background_appearance(&self, background_appearance: WindowBackgroundAppearance) {
        self.inner.set_background_appearance(background_appearance)
    }
    fn minimize(&self) {
        self.inner.minimize()
    }
    fn zoom(&self) {
        self.inner.zoom()
    }
    fn toggle_fullscreen(&self) {
        self.inner.toggle_fullscreen()
    }
    fn is_fullscreen(&self) -> bool {
        self.inner.is_fullscreen()
    }
    fn frame_waker(&self) -> Option<Rc<dyn Fn()>> {
        self.inner.frame_waker()
    }
    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.inner.on_request_frame(callback)
    }
    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.inner.on_active_status_change(callback)
    }
    fn on_visibility_change(&self, callback: Box<dyn FnMut(WindowVisibility)>) {
        self.inner.on_visibility_change(callback)
    }
    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.inner.on_hover_status_change(callback)
    }
    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_moved(callback)
    }
    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.inner.on_should_close(callback)
    }
    fn on_hit_test_window_control(&self, callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
        self.inner.on_hit_test_window_control(callback)
    }
    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.inner.on_close(callback)
    }
    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_appearance_changed(callback)
    }
    fn on_button_layout_changed(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_button_layout_changed(callback)
    }
    fn draw(&self, scene: &Scene) {
        self.inner.draw(scene)
    }
    fn schedule_frame(&self) {
        self.inner.schedule_frame()
    }
    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.inner.sprite_atlas()
    }
    fn is_subpixel_rendering_supported(&self) -> bool {
        self.inner.is_subpixel_rendering_supported()
    }
    fn get_title(&self) -> String {
        self.inner.get_title()
    }
    fn tabbed_windows(&self) -> Option<Vec<SystemWindowTab>> {
        self.inner.tabbed_windows()
    }
    fn tab_bar_visible(&self) -> bool {
        self.inner.tab_bar_visible()
    }
    fn set_edited(&mut self, edited: bool) {
        self.inner.set_edited(edited)
    }
    fn set_document_path(&self, path: Option<&Path>) {
        self.inner.set_document_path(path)
    }
    fn toggle_simple_fullscreen(&self) {
        self.inner.toggle_simple_fullscreen()
    }
    fn is_simple_fullscreen(&self) -> bool {
        self.inner.is_simple_fullscreen()
    }
    fn show_character_palette(&self) {
        self.inner.show_character_palette()
    }
    fn titlebar_double_click(&self, is_resizable: bool, is_minimizable: bool) {
        self.inner.titlebar_double_click(is_resizable, is_minimizable)
    }
    fn on_move_tab_to_new_window(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_move_tab_to_new_window(callback)
    }
    fn on_merge_all_windows(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_merge_all_windows(callback)
    }
    fn on_select_previous_tab(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_select_previous_tab(callback)
    }
    fn on_select_next_tab(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_select_next_tab(callback)
    }
    fn on_toggle_tab_bar(&self, callback: Box<dyn FnMut()>) {
        self.inner.on_toggle_tab_bar(callback)
    }
    fn merge_all_windows(&self) {
        self.inner.merge_all_windows()
    }
    fn move_tab_to_new_window(&self) {
        self.inner.move_tab_to_new_window()
    }
    fn toggle_window_tab_overview(&self) {
        self.inner.toggle_window_tab_overview()
    }
    fn set_tabbing_identifier(&self, identifier: Option<String>) {
        self.inner.set_tabbing_identifier(identifier)
    }
    fn native_window_state(&self) -> Option<Vec<u8>> {
        self.inner.native_window_state()
    }
    fn restore_native_window_state(&self, state: &[u8]) {
        self.inner.restore_native_window_state(state)
    }
    #[cfg(target_os = "windows")]
    fn get_raw_handle(&self) -> windows::Win32::Foundation::HWND {
        self.inner.get_raw_handle()
    }
    fn request_decorations(&self, decorations: WindowDecorations) {
        self.inner.request_decorations(decorations)
    }
    fn start_window_move(&self) {
        self.inner.start_window_move()
    }
    fn can_start_external_drag(&self) -> bool {
        self.inner.can_start_external_drag()
    }
    fn start_external_drag(&self, payload: &ExternalDragPayload) -> bool {
        self.inner.start_external_drag(payload)
    }
    fn start_window_resize(&self, edge: ResizeEdge) {
        self.inner.start_window_resize(edge)
    }
    fn window_decorations(&self) -> Decorations {
        self.inner.window_decorations()
    }
    fn set_app_id(&mut self, app_id: &str) {
        self.inner.set_app_id(app_id)
    }
    fn map_window(&mut self) -> Result<()> {
        self.inner.map_window()
    }
    fn window_controls(&self) -> WindowControls {
        self.inner.window_controls()
    }
    fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.inner.gpu_specs()
    }
    fn insets(&self) -> WindowInsets {
        self.inner.insets()
    }
    fn on_insets_changed(&self, callback: Box<dyn FnMut(WindowInsets)>) {
        self.inner.on_insets_changed(callback)
    }
    fn set_back_handler(&self, callback: Box<dyn FnMut()>) {
        self.inner.set_back_handler(callback)
    }
    fn set_back_enabled(&self, enabled: bool) {
        self.inner.set_back_enabled(enabled)
    }
    fn show_soft_keyboard(&self) {
        self.inner.show_soft_keyboard()
    }
    fn hide_soft_keyboard(&self) {
        self.inner.hide_soft_keyboard()
    }
    fn text_input_state_changed(&self, change: TextInputStateChange) {
        self.inner.text_input_state_changed(change)
    }
    fn play_system_bell(&self) {
        self.inner.play_system_bell()
    }
    fn a11y_init(&self, callbacks: A11yCallbacks) {
        self.inner.a11y_init(callbacks)
    }
    fn a11y_tree_update(&self, tree_update: gpui::accesskit::TreeUpdate) {
        self.inner.a11y_tree_update(tree_update)
    }
    fn a11y_update_window_bounds(&self) {
        self.inner.a11y_update_window_bounds()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_steps_in_tens_and_clamps_at_both_bounds() {
        assert_eq!(next(100, 1), 110);
        assert_eq!(next(100, -1), 90);
        assert_eq!(next(100, 2), 120);
        assert_eq!(next(50, -1), 50);
        assert_eq!(next(200, 1), 200);
        assert_eq!(next(30, 1), 50);
        assert_eq!(next(260, -1), 200);
    }

    #[test]
    fn events_come_in_divided_by_the_zoom() {
        let down = PlatformInput::MouseDown(gpui::MouseDownEvent {
            position: point(gpui::px(300.), gpui::px(150.)),
            ..Default::default()
        });
        let PlatformInput::MouseDown(e) = scale_input(down, 1. / 1.5) else { unreachable!() };
        assert_eq!(e.position, point(gpui::px(200.), gpui::px(100.)));
    }
}
