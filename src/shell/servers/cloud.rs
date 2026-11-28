//! The account's cloud environment in the Computers tab: `arakun-server` in
//! a container Arakun runs, which keeps working with the computer off. It
//! runs on CPU Arakun pays for, so an account asks for access first and the
//! operator grants it by hand. With access, the tab makes the environment,
//! wakes, stops and removes it, and says how many hours are left this month
//! and why it stopped. Once it runs, it is in the list as one of the
//! account's servers, and is reached like any other.

use super::*;
use crate::cloud::{self, CloudAct};
use crate::servers_view::{CloudRow, CloudState};
use std::time::Duration;

/// How soon to ask again while a new environment starts.
const STARTING_POLL: Duration = Duration::from_secs(10);

/// The cloud environment, as last asked.
#[derive(Default)]
pub(in crate::shell) struct Cloud {
    /// The service and session it was asked with; an answer for another one
    /// is dropped.
    asked: Option<(String, String)>,
    row: CloudRow,
    _task: Option<Task<()>>,
    _poll: Option<Task<()>>,
}

/// Whether a new environment is still on its first start, which the
/// service does in the background.
fn starting(state: &CloudState) -> bool {
    matches!(state, CloudState::Ready { machine: Some(m), .. }
        if !m.running && !m.snapshot && m.stopped.is_none())
}

/// What the account says now: where its request stands, or, with access,
/// its environment and this month's hours. Hidden where the service has
/// none set up.
fn read(base: &str, token: &str) -> Result<CloudState, cloud::Error> {
    // The fixture's session is made up: the service would answer 401.
    if crate::arakun_teams::fixture().is_some() {
        return Ok(fixture());
    }
    let access = cloud::cloud_access(base, token)?;
    // Granted without GitHub (unlinked since, or granted by hand): the
    // environment won't start, so the tab offers the link instead.
    if access.status != "granted" || !access.github {
        return Ok(CloudState::Ask(access));
    }
    let Some(list) = cloud::cloud_machines(base, token)? else {
        return Ok(CloudState::Hidden);
    };
    // The hours are a line under it; the environment shows without them.
    let usage = cloud::cloud_usage(base, token).ok();
    Ok(CloudState::Ready {
        machine: list.into_iter().next(),
        usage,
    })
}

/// A made-up environment for the test fixture, stopped at the task cap.
fn fixture() -> CloudState {
    CloudState::Ready {
        machine: Some(cloud::CloudMachine {
            label: "cloudcloudcloud2".into(),
            name: "Cloud".into(),
            running: false,
            snapshot: true,
            stopped: Some("task_cap".into()),
        }),
        usage: Some(cloud::CloudUsage {
            allowance_ms: 20 * 3_600_000,
            left_ms: 13 * 3_600_000 + 1_800_000,
            task_cap_ms: 2 * 3_600_000,
            stopped: None,
            message: None,
        }),
    }
}

impl Shell {
    pub(in crate::shell) fn servers_cloud_row(&self) -> CloudRow {
        self.servers
            .as_ref()
            .map(|l| l.cloud.row.clone())
            .unwrap_or_default()
    }

    /// The label of the account's cloud environment, once it is known.
    pub(in crate::shell) fn servers_cloud_label(&self) -> Option<String> {
        match &self.servers.as_ref()?.cloud.row.state {
            CloudState::Ready {
                machine: Some(m), ..
            } => Some(m.label.clone()),
            _ => None,
        }
    }

    /// Asks the account about its cloud environment, when signed in.
    pub(in crate::shell) fn servers_ask_cloud(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = self.teams.read(cx).session();
        let Some(link) = &mut self.servers else {
            return;
        };
        let cloud = &mut link.cloud;
        if cloud.asked != session {
            cloud.row = CloudRow::default();
        }
        cloud.asked = session.clone();
        cloud._poll = None;
        let Some((base, token)) = session else {
            cloud._task = None;
            return;
        };
        let asked = (base.clone(), token.clone());
        cloud._task = Some(cx.spawn_in(window, async move |this, cx| {
            let read = cx
                .background_spawn(async move { read(&base, &token) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.servers_cloud_read(asked, read, window, cx)
            });
        }));
    }

    fn servers_cloud_read(
        &mut self,
        asked: (String, String),
        read: Result<CloudState, cloud::Error>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = &mut self.servers else {
            return;
        };
        let cloud = &mut link.cloud;
        if cloud.asked.as_ref() != Some(&asked) {
            return;
        }
        cloud._task = None;
        match read {
            Ok(state) => {
                cloud.row.state = state;
                cloud.row.error = None;
            }
            Err(cloud::Error::SignedOut) => {
                self.teams.update(cx, |t, cx| t.session_ended(cx));
                return;
            }
            // What was shown stays, under the reason it could not be asked.
            Err(e) => cloud.row.error = Some(e.to_string()),
        }
        if starting(&cloud.row.state) {
            cloud._poll = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(STARTING_POLL).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.servers_ask_cloud(window, cx);
                    // It joins the list of servers once it runs.
                    this.servers_list_machines(window, cx);
                });
            }));
        }
        self.sync_servers_view(cx);
    }

    /// Asks for access, makes the environment, or wakes, stops or removes
    /// it, then asks the account again.
    pub(in crate::shell) fn servers_cloud_event(
        &mut self,
        ev: &ServersEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((base, token)) = self.teams.read(cx).session() else {
            return;
        };
        let Some(link) = &mut self.servers else {
            return;
        };
        if link.cloud.row.busy {
            return;
        }
        // Whether the list of servers changes with it.
        let listed = matches!(
            ev,
            ServersEvent::CloudMake
                | ServersEvent::Cloud {
                    act: CloudAct::Remove,
                    ..
                }
        );
        let call: Box<dyn FnOnce() -> Result<(), cloud::Error> + Send> = match ev {
            ServersEvent::CloudAsk(reason) => {
                let reason = reason.clone();
                Box::new(move || cloud::ask_cloud_access(&base, &token, &reason).map(|_| ()))
            }
            ServersEvent::CloudMake => {
                Box::new(move || cloud::make_cloud_machine(&base, &token).map(|_| ()))
            }
            ServersEvent::Cloud { label, act } => {
                let (label, act) = (label.clone(), *act);
                Box::new(move || cloud::cloud_machine_act(&base, &token, &label, act))
            }
            _ => return,
        };
        link.cloud.row.busy = true;
        link.cloud.row.error = None;
        self.sync_servers_view(cx);
        cx.spawn_in(window, async move |this, cx| {
            let done = if crate::arakun_teams::fixture().is_some() {
                Ok(())
            } else {
                cx.background_spawn(async move { call() }).await
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(link) = &mut this.servers {
                    link.cloud.row.busy = false;
                    match done {
                        Err(cloud::Error::SignedOut) => {
                            this.teams.update(cx, |t, cx| t.session_ended(cx));
                            return;
                        }
                        Err(e) => link.cloud.row.error = Some(e.to_string()),
                        Ok(()) => {}
                    }
                }
                this.servers_ask_cloud(window, cx);
                if listed {
                    this.servers_list_machines(window, cx);
                }
                this.sync_servers_view(cx);
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(running: bool, snapshot: bool, stopped: Option<&str>) -> CloudState {
        CloudState::Ready {
            machine: Some(cloud::CloudMachine {
                label: "abcdefghijklmnop".into(),
                name: "Cloud".into(),
                running,
                snapshot,
                stopped: stopped.map(str::to_string),
            }),
            usage: None,
        }
    }

    #[test]
    fn only_a_first_start_is_asked_about_again_soon() {
        assert!(starting(&machine(false, false, None)));
        assert!(!starting(&machine(true, false, None)));
        assert!(!starting(&machine(false, true, None)));
        assert!(!starting(&machine(false, false, Some("held"))));
        assert!(!starting(&CloudState::Hidden));
        assert!(!starting(&CloudState::Ready {
            machine: None,
            usage: None
        }));
    }
}
