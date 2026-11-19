//! The shell's side of `crate::update`: a check a little after launch and
//! every few hours after it, and a notice once a new version is downloaded.

use super::*;
use crate::island::AlertKind;
use crate::update::{self, Outcome};

/// The first check waits this long, so it doesn't compete with the launch.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(60);

impl Shell {
    pub(super) fn start_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !update::release_build() {
            return;
        }
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
}
