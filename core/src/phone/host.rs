//! What a device asks of a session, handled once for every host. The app
//! (for its phones) and the server daemon (for phones and the app) both hold
//! sessions and answer `ClientMsg`s. Where they act the same, the work is
//! here, and a host only says how it finds and keeps its sessions
//! (`SessionHost`). What only one of them does, or does its own way, is
//! handed back to it by `dispatch`.

use std::path::PathBuf;

use gpui::{App, Context, Entity};
use serde_json::json;

use super::{ClientId, ClientMsg, wire};
use crate::agent::{Setting, SettingOption, Thread};

/// The settings a device can pick: the model, then the reasoning (its
/// choices depend on the model). A device that could pick a permission mode
/// would turn the asking off for the session it runs.
pub const DEVICE_SETTINGS: [Setting; 2] = [Setting::Model, Setting::Effort];

/// The setting `key` names, when it is one a device may pick.
pub fn device_setting(key: &str) -> Option<Setting> {
    Setting::from_key(key).filter(|s| DEVICE_SETTINGS.contains(s))
}

/// A running session's settings a device can change, with their choices.
pub fn session_settings(t: &Thread) -> Vec<(Setting, (String, Vec<SettingOption>))> {
    DEVICE_SETTINGS
        .into_iter()
        .filter_map(|s| Some((s, t.setting(s).filter(|(_, o)| !o.is_empty())?)))
        .collect()
}

/// An error about session `session`, which a device shows in that session.
/// A device that does not look for `id` shows it as any other error.
pub fn session_error(session: &str, message: &str) -> String {
    json!({ "type": "error", "id": session, "message": message }).to_string()
}

/// Answers the questionnaire a session waits on: one answer for each question.
pub fn answer_question(
    thread: &Entity<Thread>,
    answers: Vec<String>,
    cx: &mut App,
) -> Result<(), String> {
    let fits = thread
        .read(cx)
        .pending_question()
        .is_some_and(|q| q.questions.len() == answers.len());
    if !fits {
        return Err("that question is gone".into());
    }
    thread.update(cx, |t, cx| t.answer_question(answers, cx));
    Ok(())
}

/// What `dispatch` needs of the side that holds the sessions: where they
/// are and how it keeps them. Session ids are the ones the device was given.
pub trait SessionHost {
    /// The entity whose context the host works in.
    type Owner: 'static;

    /// Sends `msg` to the device `client`.
    fn reply(&mut self, client: ClientId, msg: String);

    /// Whether the session `session` is one of this host's.
    fn has_session(&self, session: &str) -> bool;

    /// The session's thread, when its agent runs.
    fn thread(&self, session: &str) -> Option<Entity<Thread>>;

    /// The session's thread, its agent started when it was not, for what a
    /// device reads of it. `None` when it could not start.
    fn live_thread(
        &mut self,
        session: &str,
        cx: &mut Context<Self::Owner>,
    ) -> Option<Entity<Thread>>;

    /// The thread whose queue a device acts on; `starts_turn` when that
    /// would send a prompt.
    fn queue_thread(
        &mut self,
        session: &str,
        _starts_turn: bool,
        _cx: &mut Context<Self::Owner>,
    ) -> Result<Entity<Thread>, String> {
        self.thread(session)
            .ok_or_else(|| "that session is not running".into())
    }

    /// Stops the session's running turn. `keep_queue` asks to pause the
    /// follow-ups instead of dropping them; the host decides what it does.
    fn stop(&mut self, thread: &Entity<Thread>, keep_queue: bool, cx: &mut Context<Self::Owner>);

    /// A device picked `value` for the session's setting `s`: the session's
    /// folder remembers it for its next session of that agent.
    fn remember_setting(
        &mut self,
        session: &str,
        s: Setting,
        value: &str,
        cx: &mut Context<Self::Owner>,
    );

    /// Whether what devices say reaches this host encrypted end to end.
    fn private(&self) -> bool;

    /// Keeps a piece of a photo, or of any `file`; the saved file once the
    /// last piece landed.
    fn upload_piece(
        &mut self,
        upload: &str,
        name: &str,
        file: bool,
        part: usize,
        of: usize,
        data: &str,
    ) -> Option<PathBuf>;

    /// Whether the device `client` shows its screen now.
    fn set_visible(&mut self, client: ClientId, visible: bool);
}

/// Handles what every host does the same way with a device's message, and
/// hands back the rest for the host's own handling.
pub fn dispatch<H: SessionHost>(
    host: &mut H,
    client: ClientId,
    msg: ClientMsg,
    cx: &mut Context<H::Owner>,
) -> Option<ClientMsg> {
    match msg {
        ClientMsg::Upload {
            session: _,
            upload,
            name,
            file,
            part,
            of,
            data,
        } => {
            if let Some(path) = host.upload_piece(&upload, &name, file, part, of, &data) {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default();
                let reply = json!({ "type": "uploaded", "upload": upload, "name": name });
                host.reply(client, reply.to_string());
            }
        }
        ClientMsg::Settings { session } => {
            if !host.has_session(&session) {
                return None;
            }
            let list = host
                .live_thread(&session, cx)
                .map(|thread| session_settings(thread.read(cx)))
                .unwrap_or_default();
            host.reply(client, settings_message(&session, list));
        }
        ClientMsg::Setting {
            session,
            key,
            value,
        } => {
            let Some(s) = device_setting(&key) else {
                let e = "that setting is not one a device changes";
                host.reply(client, session_error(&session, e));
                return None;
            };
            let Some(thread) = host.thread(&session) else {
                let e = "that session is not running";
                host.reply(client, session_error(&session, e));
                return None;
            };
            thread.update(cx, |t, cx| t.set_setting(s, &value, cx));
            host.remember_setting(&session, s, &value, cx);
            let list = session_settings(thread.read(cx));
            host.reply(client, settings_message(&session, list));
        }
        ClientMsg::Cancel {
            session,
            keep_queue,
        } => {
            if let Some(thread) = host.thread(&session) {
                host.stop(&thread, keep_queue, cx);
            }
        }
        ClientMsg::Unqueue {
            session,
            index,
            text,
            now,
        } => match host.queue_thread(&session, now, cx) {
            Ok(thread) => thread.update(cx, |t, cx| t.unqueue(index, &text, now, cx)),
            Err(e) => host.reply(client, session_error(&session, &e)),
        },
        ClientMsg::ResumeQueue { session } => match host.queue_thread(&session, true, cx) {
            Ok(thread) => thread.update(cx, |t, cx| t.resume_queue(cx)),
            Err(e) => host.reply(client, session_error(&session, &e)),
        },
        ClientMsg::Permission { session, option } => {
            if let Some(thread) = host.thread(&session) {
                thread.update(cx, |t, cx| t.respond_permission(&option, cx));
            }
        }
        ClientMsg::Answer { session, answers } => {
            if let Some(thread) = host.thread(&session)
                && let Err(e) = answer_question(&thread, answers, cx)
            {
                host.reply(client, session_error(&session, &e));
            }
        }
        ClientMsg::SkipQuestion { session } => {
            if let Some(thread) = host.thread(&session) {
                thread.update(cx, |t, cx| t.skip_question(cx));
            }
        }
        ClientMsg::Secret {
            session,
            id,
            values,
        } => {
            let private = host.private();
            let answered = match host.thread(&session) {
                Some(thread) => thread.update(cx, |t, cx| t.answer_secret(id, values, private, cx)),
                None => Err("that session is not running".into()),
            };
            if let Err(e) = answered {
                host.reply(client, session_error(&session, &e));
            }
        }
        ClientMsg::Visible { visible } => host.set_visible(client, visible),
        ClientMsg::Ping => {}
        other => return Some(other),
    }
    None
}

/// The `settings` answer: a session's settings a device can change.
fn settings_message(session: &str, list: Vec<(Setting, (String, Vec<SettingOption>))>) -> String {
    json!({ "type": "settings", "id": session, "settings": wire::settings(list) }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_picks_only_the_model_and_the_reasoning() {
        assert_eq!(device_setting("modelId"), Some(Setting::Model));
        assert_eq!(device_setting("reasoningEffort"), Some(Setting::Effort));
        assert_eq!(device_setting("nope"), None);
        for s in Setting::ALL {
            if !DEVICE_SETTINGS.contains(&s) {
                assert_eq!(device_setting(s.key()), None, "{s:?}");
            }
        }
    }

    #[test]
    fn a_session_error_names_its_session() {
        let e: serde_json::Value = serde_json::from_str(&session_error("s1", "gone")).unwrap();
        assert_eq!(e["type"], "error");
        assert_eq!(e["id"], "s1");
        assert_eq!(e["message"], "gone");
    }
}
