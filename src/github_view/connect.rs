//! "Connect arakun-pilot": the Worker's address and tokens typed in a form,
//! checked against the Worker before they are saved, so nobody has to write
//! `client.json` by hand.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
};

use crate::github::pilot::{self, Config};
use crate::github::pilot_setup::{self, Draft};
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, ButtonKind, Type, Typography as _};

pub enum PilotDialogEvent {
    /// Saved or removed: pilot reads its settings again.
    Changed,
    Dismissed,
}

pub struct PilotDialog {
    /// The settings on disk when the form opened.
    saved: Option<Config>,
    /// A settings file there that can't be used: still one to fix or
    /// disconnect, not "Connect".
    broken: bool,
    /// "Remove the saved admin token" was clicked: saving drops it.
    drop_admin: bool,
    url: Entity<TextInput>,
    token: Entity<TextInput>,
    admin: Entity<TextInput>,
    busy: bool,
    error: Option<SharedString>,
    /// "Disconnect" waits for its second click.
    confirm_remove: bool,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PilotDialogEvent> for PilotDialog {}

impl PilotDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // read_config, not load_config: a broken file must open as itself,
        // with Disconnect and why it can't be used.
        let (saved, broken) = match pilot::read_config() {
            Ok(saved) => (saved, None),
            Err(e) => (None, Some(e)),
        };
        let url = cx.new(|cx| {
            let mut input = TextInput::new("https://pilot.example.workers.dev", cx).mono();
            if let Some(s) = &saved {
                input.set_text(s.url.clone(), cx);
            }
            input
        });
        let token = cx.new(|cx| TextInput::new("", cx).mono().secret());
        let admin = cx.new(|cx| TextInput::new("", cx).mono().secret());
        let follow = |this: &mut Self,
                      input: &Entity<TextInput>,
                      ev: &InputEvent,
                      window: &mut Window,
                      cx: &mut Context<Self>| match ev {
            InputEvent::Submit => this.connect(cx),
            // Tab: address, token, admin token, and round again.
            InputEvent::Complete => {
                let next = if *input == this.url {
                    &this.token
                } else if *input == this.token {
                    &this.admin
                } else {
                    &this.url
                };
                window.focus(&next.focus_handle(cx), cx);
            }
            InputEvent::Cancel => {
                if !this.busy {
                    cx.emit(PilotDialogEvent::Dismissed);
                }
            }
            InputEvent::Changed => {
                this.error = None;
                this.confirm_remove = false;
                if *input == this.url {
                    this.hints(cx);
                }
                cx.notify();
            }
            _ => {}
        };
        let subs = vec![
            cx.subscribe_in(&url, window, follow),
            cx.subscribe_in(&token, window, follow),
            cx.subscribe_in(&admin, window, follow),
        ];
        // Connected: the token is what changes most; else start at the top.
        let first = if saved.is_some() { &token } else { &url };
        window.focus(&first.focus_handle(cx), cx);
        let mut this = Self {
            saved,
            broken: broken.is_some(),
            drop_admin: false,
            url,
            token,
            admin,
            busy: false,
            error: broken.map(|e| format!("The saved settings can't be used: {e}.").into()),
            confirm_remove: false,
            _subs: subs,
        };
        this.hints(cx);
        this
    }

    /// Whether a save keeps something running: the dialog stays until it ends.
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Whether the address typed is the saved one: only then do empty
    /// token boxes keep the saved tokens, which never go to another host.
    fn same_address(&self, cx: &App) -> bool {
        let typed = self.url.read(cx).text().trim().trim_end_matches('/');
        self.saved.as_ref().is_some_and(|s| s.url == typed)
    }

    fn saved_admin(&self, cx: &App) -> bool {
        self.same_address(cx) && self.saved.as_ref().is_some_and(|s| s.admin_token.is_some())
    }

    /// The token boxes' hints for the address typed now.
    fn hints(&mut self, cx: &mut Context<Self>) {
        let same = self.same_address(cx);
        let token = if same {
            "Saved. Leave empty to keep it"
        } else {
            "The Worker's read token"
        };
        let had_admin = self.saved.as_ref().is_some_and(|s| s.admin_token.is_some());
        let admin = if self.saved_admin(cx) && self.drop_admin {
            "Removed when you save"
        } else if self.saved_admin(cx) {
            "Saved. Leave empty to keep it"
        } else if had_admin {
            "Type it again: the saved one stays with the old address"
        } else {
            "Optional: lets Arakun send pilot tasks"
        };
        self.token.update(cx, |i, cx| i.set_placeholder(token, cx));
        self.admin.update(cx, |i, cx| i.set_placeholder(admin, cx));
    }

    fn draft(&self, cx: &App) -> Draft {
        let admin = self.admin.read(cx).text().trim().to_string();
        Draft {
            url: self.url.read(cx).text().to_string(),
            token: self.token.read(cx).text().to_string(),
            // Typed wins; else empty keeps the saved one, unless it was removed.
            admin_token: if !admin.is_empty() {
                Some(admin)
            } else if self.drop_admin {
                Some(String::new())
            } else {
                None
            },
        }
    }

    fn toggle_drop_admin(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.drop_admin = !self.drop_admin;
        self.hints(cx);
        cx.notify();
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let cfg = match pilot_setup::settings(&self.draft(cx), self.saved.as_ref()) {
            Ok(cfg) => cfg,
            Err(e) => {
                self.error = Some(e.into());
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.error = None;
        self.confirm_remove = false;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(crate::guard::task(async move {
                    pilot_setup::check(&cfg)?;
                    pilot_setup::save(&cfg)
                        .map(|_| ())
                        .map_err(|e| format!("Couldn't save the settings: {e}"))
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => cx.emit(PilotDialogEvent::Changed),
                    Err(e) => {
                        this.error = Some(e.into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        // A save still running would write the file back after removing it.
        if self.busy {
            return;
        }
        if !self.confirm_remove {
            self.confirm_remove = true;
            cx.notify();
            return;
        }
        match pilot_setup::remove() {
            Ok(_) => cx.emit(PilotDialogEvent::Changed),
            Err(e) => {
                self.confirm_remove = false;
                self.error = Some(format!("Couldn't remove the settings: {e}").into());
                cx.notify();
            }
        }
    }
}

impl Focusable for PilotDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.url.focus_handle(cx)
    }
}

/// A labeled text box of the form, with a line under it when there is one.
fn labeled(
    label: &'static str,
    input: &Entity<TextInput>,
    note: Option<&'static str>,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(ui::sp(6.))
        .child(ui::text(Type::Label, theme::text_muted(), label))
        .child(
            ui::field()
                .bg(theme::background())
                .child(div().flex_1().min_w_0().child(input.clone())),
        )
        .when_some(note, |d, note| {
            d.child(ui::text(Type::Caption, theme::text_subtle(), note))
        })
}

impl Render for PilotDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let free = !self.busy;
        let connected = self.saved.is_some() || self.broken;
        let saved_admin = self.saved_admin(cx);
        let title = if connected {
            "arakun-pilot"
        } else {
            "Connect arakun-pilot"
        };
        let sub = if connected {
            "Your pull request agent. Change its address or tokens, or disconnect it."
        } else {
            "Your pull request agent on Cloudflare. With it, the GitHub tab shows its board and its cards."
        };
        let path = pilot::config_file();
        let close = cx.listener(|this, _, _, cx| {
            if !this.busy {
                cx.emit(PilotDialogEvent::Dismissed)
            }
        });
        ui::modal_panel("pilot-connect", 460.)
            .child(ui::modal_header(
                title,
                Some(sub.into()),
                ui::close_button("pilot-close").on_click(close),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(14.))
                    .p(ui::sp(16.))
                    .child(labeled(
                        "Worker address",
                        &self.url,
                        Some("https://, or http:// on localhost only: the tokens never cross a network unencrypted."),
                    ))
                    .child(labeled("Token", &self.token, None))
                    .child(
                        labeled(
                            "Admin token",
                            &self.admin,
                            Some("Only for sending pilot tasks, from the palette or a pull request's sheet."),
                        )
                        // The only way to drop the admin token and keep the rest.
                        .when(saved_admin, |d| {
                            d.child(
                                div()
                                    .id("pilot-drop-admin")
                                    .ty(Type::Caption)
                                    .text_color(theme::link())
                                    .cursor_pointer()
                                    .hover(|s| s.underline())
                                    .child(if self.drop_admin {
                                        "Keep the saved admin token"
                                    } else {
                                        "Remove the saved admin token"
                                    })
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.toggle_drop_admin(cx)),
                                    ),
                            )
                        }),
                    )
                    .child(ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        format!("Saved in {}, readable only by you.", path.display()),
                    ))
                    .when_some(self.error.clone(), |d, e| {
                        d.child(ui::text(Type::Body, theme::error().into(), e).id("pilot-error"))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(8.))
                            .when(connected, |d| {
                                let label = if self.confirm_remove {
                                    "Disconnect? Click again"
                                } else {
                                    "Disconnect"
                                };
                                d.child(
                                    ui::button("pilot-remove", label, ButtonKind::Danger, free)
                                        .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                                )
                            })
                            .child(div().flex_1())
                            .child(
                                ui::button("pilot-cancel", "Cancel", ButtonKind::Ghost, free)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if !this.busy {
                                            cx.emit(PilotDialogEvent::Dismissed)
                                        }
                                    })),
                            )
                            .child(
                                ui::button(
                                    "pilot-save",
                                    if self.busy {
                                        "Checking…"
                                    } else if connected {
                                        "Check and save"
                                    } else {
                                        "Check and connect"
                                    },
                                    ButtonKind::Primary,
                                    free,
                                )
                                .when(self.busy, |d| {
                                    d.child(ui::spinner("pilot-spin", 12., theme::background()))
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                            ),
                    ),
            )
    }
}
