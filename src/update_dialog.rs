//! The dialog the `update-policy` feature flag opens (`update::policy`): it
//! asks the user to restart into the new release, or, when the flag forces
//! it, stays until they do. It shows the updater's status while the release
//! downloads, so it can open before the download is done.

use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, prelude::FluentBuilder as _, px,
};

use crate::theme;
use crate::ui::{self, ButtonKind};
use crate::update::{self, Policy, Push, Status};

pub enum UpdateEvent {
    /// Restart into the downloaded release (quit to install it on Windows).
    Restart,
    /// Look for the release again after a failed download.
    Retry,
    /// Asked, not forced: the user put it off.
    Later,
    /// Forced, and the user would rather quit than update now.
    Quit,
}

pub struct UpdateDialog {
    focus: FocusHandle,
    pub policy: Policy,
    /// Holds back the window's shortcuts while the dialog is open (see `new`).
    _keys: gpui::Subscription,
}

impl EventEmitter<UpdateEvent> for UpdateDialog {}

impl UpdateDialog {
    pub fn new(policy: Policy, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // The dialog covers the window, but a shortcut would still reach the
        // Shell's actions behind it (a new environment, the palette). A key
        // stopped here reaches neither a binding nor a key listener, so the
        // keys the dialog uses itself go through.
        let ours = window.window_handle();
        let _keys = cx.intercept_keystrokes(move |ev, window, cx| {
            if window.window_handle() == ours && !passes(&ev.keystroke) {
                cx.stop_propagation();
            }
        });
        Self {
            focus,
            policy,
            _keys,
        }
    }

    pub fn forced(&self) -> bool {
        self.policy.push == Push::Force
    }
}

impl Focusable for UpdateDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Whether a key reaches the window while the dialog is open: Tab moves
/// between its buttons, Escape puts it off, Enter and Space press a button.
/// Cmd+Q, Cmd+H and Cmd+M belong to the app, not to the window behind it.
fn passes(k: &gpui::Keystroke) -> bool {
    let m = k.modifiers;
    if !m.control && !m.alt && !m.platform && !m.function {
        return matches!(k.key.as_str(), "tab" | "escape" | "enter" | "space");
    }
    cfg!(target_os = "macos")
        && m.platform
        && !m.control
        && !m.alt
        && !m.shift
        && matches!(k.key.as_str(), "q" | "h" | "m")
}

/// Only Linux and macOS can start Arakun again by themselves.
const CAN_RESTART: bool = cfg!(unix);

impl Render for UpdateDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let forced = self.forced();
        let status = update::status();
        let title = if forced {
            "Update Arakun to keep using it"
        } else {
            "A new version of Arakun is ready"
        };
        let why = if forced {
            "This version has a problem serious enough that it should not be used."
        } else {
            "We ask everyone on this version to update."
        };
        let (label, enabled, event) = match &status {
            // Downloaded for the nightly channel the user has left since: it
            // would not be installed at quit (`update::will_install`).
            Status::Ready(tag) if !update::will_install(tag) => {
                ("Try again", true, UpdateEvent::Retry)
            }
            Status::Ready(_) if CAN_RESTART => ("Restart now", true, UpdateEvent::Restart),
            Status::Ready(_) => ("Quit and update", true, UpdateEvent::Restart),
            Status::Failed(_) => ("Try again", true, UpdateEvent::Retry),
            _ => ("Downloading…", false, UpdateEvent::Restart),
        };
        let event = std::cell::Cell::new(Some(event));
        let main = ui::button("update-main", label, ButtonKind::Primary, enabled).on_click(
            cx.listener(move |_, _, _, cx| {
                if enabled && let Some(ev) = event.take() {
                    cx.emit(ev);
                }
            }),
        );
        let secondary = if forced {
            ui::button("update-quit", "Quit Arakun", ButtonKind::Ghost, true)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(UpdateEvent::Quit)))
        } else {
            ui::button("update-later", "Later", ButtonKind::Ghost, true)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(UpdateEvent::Later)))
        };
        let close: gpui::AnyElement = if forced {
            div().into_any_element()
        } else {
            ui::close_button("update-close")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(UpdateEvent::Later)))
                .into_any_element()
        };
        let message: Option<SharedString> = self.policy.message.clone().map(Into::into);
        ui::modal_panel("update-policy", 420.)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(move |_, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" && !forced {
                    cx.emit(UpdateEvent::Later);
                }
            }))
            .child(ui::modal_header(title, Some(why.into()), close))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(12.))
                    .p(ui::sp(16.))
                    .text_size(px(ui::TEXT_SM))
                    .line_height(px(18.))
                    .text_color(theme::text_muted())
                    .when_some(message, |d, m| d.child(m))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(match status {
                                Status::Failed(_) => theme::danger().into(),
                                _ => theme::text_subtle(),
                            })
                            .child(status.text()),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(ui::sp(8.))
                            .pt(ui::sp(4.))
                            .child(secondary)
                            .child(main),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> gpui::Keystroke {
        gpui::Keystroke::parse(s).unwrap()
    }

    #[test]
    fn only_the_dialogs_own_keys_pass() {
        for k in ["tab", "shift-tab", "escape", "enter", "space"] {
            assert!(passes(&key(k)), "{k}");
        }
        for k in [
            "a",
            "ctrl-tab",
            "cmd-n",
            "cmd-k",
            "alt-enter",
            "cmd-shift-q",
        ] {
            assert!(!passes(&key(k)), "{k}");
        }
        assert_eq!(passes(&key("cmd-q")), cfg!(target_os = "macos"));
    }
}
