//! The shell's side of `crate::update`: a check a little after launch and
//! every few hours after it, and a notice once a new version is downloaded.
//! Also the `update-policy` feature flag (`update::policy`), read soon after
//! launch and every hour: it can ask this version to update, or force it to.

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::island::AlertKind;
use crate::update::{self, Outcome, Policy, Push};
use crate::update_dialog::{UpdateDialog, UpdateEvent};

/// The first check waits this long, so it doesn't compete with the launch.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(60);
/// The flag is read sooner: a version told to stop should stop early.
const FIRST_POLICY_AFTER: Duration = Duration::from_secs(5);
/// How often the flag is read after that.
const POLICY_EVERY: Duration = Duration::from_secs(60 * 60);

/// The user put off a requested update: not asked again until the next launch.
static PUT_OFF: AtomicBool = AtomicBool::new(false);

impl Shell {
    pub(super) fn start_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !update::release_build() {
            return;
        }
        self.start_update_policy(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let mut wait = FIRST_CHECK_AFTER;
            loop {
                cx.background_executor().timer(wait).await;
                wait = update::CHECK_EVERY;
                // Read each time, so the switch in Settings takes effect without a restart.
                if !update::automatic() {
                    continue;
                }
                let result = cx
                    .background_spawn(crate::guard::task(async { update::check() }))
                    .await;
                let alive = this
                    .update(cx, |this, cx| {
                        match result {
                            // The policy's dialog says it already.
                            Ok(Outcome::Staged(_, true)) if this.update_dialog.is_some() => {}
                            Ok(Outcome::Staged(tag, true)) => this.notify(
                                AlertKind::Info,
                                &format!("Arakun {tag} is ready"),
                                "It is installed when you quit Arakun.",
                                None,
                                cx,
                            ),
                            Ok(_) => {}
                            Err(e) => eprintln!("arakun: update: {e}"),
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    fn start_update_policy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !crate::flags::available() {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let mut wait = FIRST_POLICY_AFTER;
            loop {
                cx.background_executor().timer(wait).await;
                wait = POLICY_EVERY;
                let policy = cx
                    .background_spawn(crate::guard::task(async { update::policy() }))
                    .await;
                let policy = match policy {
                    Ok(Some(policy)) => policy,
                    Ok(None) => continue,
                    Err(e) => {
                        eprintln!("arakun: update policy: {e}");
                        continue;
                    }
                };
                let alive = this
                    .update_in(cx, |this, window, cx| this.push_update(policy, window, cx))
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Acts on what the flag says: a forced update opens its dialog at once and
    /// downloads behind it; a requested one asks once the release is downloaded.
    fn push_update(&mut self, policy: Policy, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((dialog, _)) = &self.update_dialog {
            // Open already, unless a request now turns into a force.
            if dialog.read(cx).forced() || policy.push == Push::Request {
                return;
            }
        }
        if policy.push == Push::Request && PUT_OFF.load(Ordering::Relaxed) {
            return;
        }
        if policy.push == Push::Force {
            self.open_update_dialog(policy.clone(), window, cx);
        }
        self.fetch_update(policy, window, cx);
    }

    /// Downloads the newest release for `policy`'s dialog.
    fn fetch_update(&mut self, policy: Policy, window: &mut Window, cx: &mut Context<Self>) {
        update::set_status(update::Status::Checking);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(crate::guard::task(async { update::check() }))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(Outcome::Staged(..)) => {
                        if this.update_dialog.is_none() {
                            this.open_update_dialog(policy, window, cx);
                        }
                    }
                    // Nothing newer to install, so nothing to push: the flag
                    // matched the newest version, or no release is out yet.
                    Ok(Outcome::UpToDate) => this.close_update_dialog(window, cx),
                    // A forced dialog shows the error with Try again.
                    Err(e) => eprintln!("arakun: update: {e}"),
                }
                this.refresh_update_dialog(cx);
            });
        })
        .detach();
    }

    fn open_update_dialog(&mut self, policy: Policy, window: &mut Window, cx: &mut Context<Self>) {
        let push = match policy.push {
            Push::Request => "request",
            Push::Force => "force",
        };
        crate::telemetry::capture(crate::telemetry::events::UPDATE_ASKED, &[("push", push)]);
        // A menu left open behind it would still take Enter and Space.
        self.menu = None;
        self.env_menu_open = false;
        let dialog = cx.new(|cx| UpdateDialog::new(policy, window, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            |this, dialog, ev: &UpdateEvent, window, cx| match ev {
                UpdateEvent::Restart => {
                    // Windows installs it as Arakun quits, but can't start it again.
                    this.restarting = cfg!(unix);
                    cx.quit();
                }
                UpdateEvent::Retry => {
                    let policy = dialog.read(cx).policy.clone();
                    this.fetch_update(policy, window, cx);
                }
                UpdateEvent::Later => {
                    PUT_OFF.store(true, Ordering::Relaxed);
                    this.close_update_dialog(window, cx);
                }
                UpdateEvent::Quit => cx.quit(),
            },
        );
        self.update_dialog = Some((dialog, sub));
        cx.notify();
    }

    fn close_update_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update_dialog.take().is_none() {
            return;
        }
        if self.modal.is_none() {
            // Gives the focus back the way closing a dialog does.
            self.close_modal(window, cx);
        }
        cx.notify();
    }

    fn refresh_update_dialog(&mut self, cx: &mut Context<Self>) {
        if let Some((dialog, _)) = &self.update_dialog {
            dialog.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }
}
