//! `BrowserView` for builds without the `browser` feature: the same API and toolbar, and a
//! note instead of the page.

use std::sync::Arc;

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, RenderImage, SharedString,
    Styled as _, Window, div,
};

use super::chrome::{self, Toolbar};
use super::text_field::{TextField, TextFieldEvent};
use super::url;
use super::{BrowserEvent, ConsoleLine, HomeInfo, PageState};

const NOT_BUILT: &str = "browser tabs are not part of this build";

pub struct BrowserView {
    address: Entity<TextField>,
    focus: FocusHandle,
    url: SharedString,
    _subscription: gpui::Subscription,
}

impl EventEmitter<BrowserEvent> for BrowserView {}

impl Focusable for BrowserView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BrowserView {
    pub fn new(url: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = cx.new(|cx| TextField::new("Search or enter address", cx));
        let subscription = cx.subscribe_in(&address, window, |this, field, ev, _, cx| {
            if let TextFieldEvent::Submit = ev {
                let text = field.read(cx).text().to_string();
                this.navigate(&text, cx);
            }
        });
        let mut view = Self {
            address,
            focus: cx.focus_handle(),
            url: SharedString::default(),
            _subscription: subscription,
        };
        if !url.trim().is_empty() {
            view.navigate(&url, cx);
        }
        view
    }

    pub fn title(&self) -> SharedString {
        url::display(&self.url).into()
    }
    pub fn url(&self) -> SharedString {
        self.url.clone()
    }
    pub fn is_loading(&self) -> bool {
        false
    }
    pub fn can_go_back(&self) -> bool {
        false
    }
    pub fn can_go_forward(&self) -> bool {
        false
    }
    pub fn favicon(&self) -> Option<Arc<RenderImage>> {
        None
    }
    pub fn is_home(&self) -> bool {
        self.url.is_empty()
    }
    pub fn set_home(&mut self, _: HomeInfo, _: &mut Context<Self>) {}

    pub fn navigate(&mut self, input: &str, cx: &mut Context<Self>) {
        let target = url::from_input(input);
        self.address.update(cx, |f, cx| f.set_text(&target, cx));
        self.url = target.into();
        cx.emit(BrowserEvent::UrlChanged(self.url.clone()));
        cx.emit(BrowserEvent::TitleChanged(self.title()));
        cx.notify();
    }

    pub fn back(&mut self, _: &mut Context<Self>) {}
    pub fn forward(&mut self, _: &mut Context<Self>) {}
    pub fn reload(&mut self, _: &mut Context<Self>) {}
    pub fn stop(&mut self, _: &mut Context<Self>) {}
    pub fn set_visible(&mut self, _: bool, _: &mut Context<Self>) {}
    pub fn close(&mut self, _: &mut Context<Self>) {}
    pub fn inspect(&mut self, _: &mut Context<Self>) {}
    pub fn hold_awake(&mut self, _: bool) {}

    pub fn devtools(
        &mut self,
        _: &str,
        _: serde_json::Value,
    ) -> futures::channel::oneshot::Receiver<Result<serde_json::Value, String>> {
        let (tx, rx) = futures::channel::oneshot::channel();
        let _ = tx.send(Err(NOT_BUILT.into()));
        rx
    }

    pub fn console(&self) -> Vec<ConsoleLine> {
        Vec::new()
    }

    pub fn page_state(&self) -> PageState {
        PageState {
            error: Some(NOT_BUILT.into()),
            ..PageState::default()
        }
    }

    pub fn focus_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let field = self.address.clone();
        window.focus(&field.focus_handle(cx), cx);
        field.update(cx, |f, cx| f.select_all(cx));
    }
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let address_focused = self.address.read(cx).is_focused(window);
        div()
            .key_context("Browser")
            .flex()
            .flex_col()
            .size_full()
            .track_focus(&self.focus)
            .child(chrome::toolbar(
                Toolbar {
                    can_back: false,
                    can_forward: false,
                    can_reload: false,
                    loading: false,
                    progress: 0.,
                    address_focused,
                    favicon: None,
                    download: None,
                    on_back: Box::new(|_, _, _| {}),
                    on_forward: Box::new(|_, _, _| {}),
                    on_reload: Box::new(|_, _, _| {}),
                },
                self.address.clone(),
            ))
            .child(div().relative().flex_1().child(chrome::notice(
                "globe",
                "Browser tabs are not part of this build",
                Some(
                    if cfg!(target_os = "linux") {
                        "Rebuild Arakun with the `browser` cargo feature (on by default)."
                    } else {
                        "Browser tabs need CEF, which Arakun wires up on Linux and Windows; macOS needs an app bundle Arakun does not build yet."
                    }
                    .into(),
                ),
                None,
            )))
    }
}
