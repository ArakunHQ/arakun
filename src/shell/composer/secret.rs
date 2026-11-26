//! A secret in the draft. Sent as it is, a key goes to the model provider
//! and stays in the session, its history and on phones. So before a draft
//! that holds one goes, a notice above the text offers to save it as a
//! secret, to send it anyway, or to go back, and the secret shows as stars
//! until the notice is gone.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher as _, RandomState};
use std::ops::Range;

use super::*;
use crate::agent::secret as handles;

/// The notice on show for one environment's draft.
#[derive(Default)]
pub(in crate::shell) struct SecretNotice {
    /// The send that opened it was Ctrl+Enter, so "Send anyway" stops the
    /// running turn too.
    now: bool,
    /// Why the last save failed.
    error: Option<SharedString>,
}

/// Where a secret is in a draft.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spot {
    Text,
    /// In the large paste at this index.
    Paste(usize),
}

/// What a session was told yes to: the values sent anyway, and the handles
/// and paths saving put in their place. Kept as hashes with a key made at
/// start, so the values themselves are not held here.
#[derive(Default)]
pub(super) struct Allowed {
    key: RandomState,
    by_session: HashMap<SharedString, HashSet<u64>>,
    /// Allowed in a new session before it had an id; it moves to the
    /// session the composer writes to next in that environment.
    fresh: Option<(PathBuf, HashSet<u64>)>,
}

impl Allowed {
    fn has(&self, frame: &Frame, value: &str) -> bool {
        let h = self.key.hash_one(value);
        match &frame.session {
            Some(s) => self.by_session.get(s).is_some_and(|set| set.contains(&h)),
            None => self
                .fresh
                .as_ref()
                .is_some_and(|(env, set)| *env == frame.env && set.contains(&h)),
        }
    }

    fn allow<'a>(&mut self, frame: &Frame, values: impl IntoIterator<Item = &'a str>) {
        let hashes = values.into_iter().map(|v| self.key.hash_one(v));
        match &frame.session {
            Some(s) => self.by_session.entry(s.clone()).or_default().extend(hashes),
            None => {
                let fresh = self
                    .fresh
                    .get_or_insert_with(|| (frame.env.clone(), HashSet::new()));
                if fresh.0 != frame.env {
                    *fresh = (frame.env.clone(), HashSet::new());
                }
                fresh.1.extend(hashes);
            }
        }
    }

    /// A new session got its id: what it was told yes to goes with it.
    pub(super) fn adopt(&mut self, frame: &Frame) {
        if let Some(s) = &frame.session
            && self
                .fresh
                .as_ref()
                .is_some_and(|(env, _)| *env == frame.env)
            && let Some((_, set)) = self.fresh.take()
        {
            self.by_session.entry(s.clone()).or_default().extend(set);
        }
    }
}

/// The secrets in the typed text and the large pastes, by where they are.
fn find(d: &EnvDraft, allowed: impl Fn(&str) -> bool) -> Vec<(Spot, Range<usize>)> {
    let mut out = Vec::new();
    let mut scan = |spot: Spot, text: &str| {
        for s in crate::secrets::find(text) {
            if !allowed(&text[s.range.clone()]) {
                out.push((spot, s.range));
            }
        }
    };
    scan(Spot::Text, &d.text);
    for (i, p) in d.pastes.iter().enumerate() {
        scan(Spot::Paste(i), &p.text);
    }
    out
}

/// The text of `spot` in `d`.
fn text_of(d: &EnvDraft, spot: Spot) -> &str {
    match spot {
        Spot::Text => &d.text,
        Spot::Paste(i) => &d.pastes[i].text,
    }
}

/// `text` with `ranges` as stars, a star per byte, so every offset into it,
/// for the caret, clicks and the selection, stays where it was.
fn mask(text: &str, ranges: &[Range<usize>]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut done = 0;
    for r in ranges {
        if r.start < done || r.end > text.len() {
            continue;
        }
        out.push_str(&text[done..r.start]);
        out.extend(std::iter::repeat_n('*', r.len()));
        done = r.end;
    }
    out.push_str(&text[done..]);
    out
}

/// Puts `with[i]` in place of the secret at `found[i]`. In the typed text
/// it is an edit like any other, so the caret follows it.
fn replace(d: &mut EnvDraft, found: &[(Spot, Range<usize>)], with: &[String]) {
    // From the end, so the ranges before each edit stay true.
    let mut edits: Vec<_> = found.iter().zip(with).collect();
    edits.sort_by_key(|((spot, r), _)| std::cmp::Reverse((*spot == Spot::Text, r.start)));
    let mut touched = Vec::new();
    for ((spot, r), w) in edits {
        match *spot {
            Spot::Text => d.text.replace(r.clone(), w),
            Spot::Paste(i) => {
                d.pastes[i].text.replace_range(r.clone(), w);
                touched.push(i);
            }
        }
    }
    touched.dedup();
    for i in touched {
        let text = std::mem::take(&mut d.pastes[i].text);
        d.pastes[i] = paste::Paste::new(text);
    }
}

impl Composer {
    /// The secrets in the draft on screen that this session was not told
    /// yes to.
    fn secrets_here(&self) -> Vec<(Spot, Range<usize>)> {
        find(self.here(), |v| self.allowed.has(&self.frame, v))
    }

    /// Before the draft is sent: true, with the notice on show, when it
    /// holds a secret; false when it may go.
    pub fn hold_for_secrets(&mut self, now: bool, cx: &mut Context<Self>) -> bool {
        let found = !self.secrets_here().is_empty();
        let d = self.here_mut();
        if !found {
            d.secret_notice = None;
            return false;
        }
        let notice = d.secret_notice.get_or_insert_default();
        notice.now = now;
        self.completion = None;
        cx.notify();
        true
    }

    /// After a paste: the notice, when what came in holds a secret, so it
    /// is masked from the start.
    pub(super) fn notice_pasted_secret(&mut self, pasted: &str) {
        if crate::secrets::holds(pasted) && !self.secrets_here().is_empty() {
            self.here_mut().secret_notice.get_or_insert_default();
        }
    }

    pub(super) fn secret_notice_open(&self) -> bool {
        self.here().secret_notice.is_some()
    }

    /// Cancel: the notice goes, and the draft stays as it is.
    pub(super) fn close_secret_notice(&mut self, cx: &mut Context<Self>) {
        self.here_mut().secret_notice = None;
        cx.notify();
    }

    /// Send anyway: these values are not asked about again in this session.
    fn send_anyway(&mut self, cx: &mut Context<Self>) {
        let found = self.secrets_here();
        let d = self.here();
        let values: Vec<String> = found
            .iter()
            .map(|(spot, r)| text_of(d, *spot)[r.clone()].to_string())
            .collect();
        self.allowed
            .allow(&self.frame, values.iter().map(String::as_str));
        let now = self.here_mut().secret_notice.take().is_some_and(|n| n.now);
        cx.emit(ComposerEvent::Submit { now });
        cx.notify();
    }

    /// Whether saving gives the agent a handle (it has `run_with_secrets`)
    /// rather than a private file's path.
    fn saves_as_handle(&self) -> bool {
        handles::offered_to(self.frame.agent)
    }

    /// Save as a secret: each value goes where the secret card puts what is
    /// typed into it, and a handle or a private file's path takes its place
    /// in the draft. The draft is not sent, so the user sees what will go.
    fn save_secrets(&mut self, cx: &mut Context<Self>) {
        let found = self.secrets_here();
        let d = self.here();
        let values: Vec<(String, String)> = found
            .iter()
            .enumerate()
            .map(|(i, (spot, r))| {
                let name = format!("pasted-secret-{}", i + 1);
                (name, text_of(d, *spot)[r.clone()].to_string())
            })
            .collect();
        let saved = if self.saves_as_handle() {
            let session = self.frame.session.as_deref().unwrap_or_default();
            // The card number only feeds the handle's HMAC, next to a nonce.
            handles::keep(&self.frame.env, &format!("composer:{session}"), 0, values)
        } else {
            handles::file(values)
        };
        match saved {
            Ok(saved) => {
                let with: Vec<String> = saved.into_iter().map(|(_, w)| w).collect();
                // A handle after a secret's name (`API_KEY=secret-ref:…`) reads
                // as a secret to the detector; it is not one.
                self.allowed
                    .allow(&self.frame, with.iter().map(String::as_str));
                let d = self.here_mut();
                replace(d, &found, &with);
                d.secret_notice = None;
                self.changed(cx);
            }
            Err(e) => {
                if let Some(n) = &mut self.here_mut().secret_notice {
                    n.error = Some(e.into());
                }
            }
        }
        cx.notify();
    }

    /// Where the typed text holds the secrets the notice is about, to mask
    /// them; empty when no notice is on show.
    pub(super) fn masked_ranges(&self) -> Vec<Range<usize>> {
        if !self.secret_notice_open() {
            return Vec::new();
        }
        self.secrets_here()
            .into_iter()
            .filter(|(spot, _)| *spot == Spot::Text)
            .map(|(_, r)| r)
            .collect()
    }

    /// The draft's text as the composer shows it: masked where it holds a
    /// secret the notice is about.
    pub(super) fn shown_text(text: &str, masked: &[Range<usize>]) -> String {
        if masked.is_empty() {
            text.to_string()
        } else {
            mask(text, masked)
        }
    }

    /// The notice above the text: what was found, and the three choices.
    pub(super) fn render_secret_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notice = self.here().secret_notice.as_ref()?;
        let count = self.secrets_here().len();
        if count == 0 {
            return None;
        }
        let what = if count == 1 {
            "This message holds what looks like a secret.".to_string()
        } else {
            format!("This message holds what looks like {count} secrets.")
        };
        // A server's session runs elsewhere: a handle or a file kept on this
        // computer would mean nothing to its agent.
        let can_save = !self.frame.remote;
        let then = match (can_save, self.saves_as_handle()) {
            (false, _) => "Sent as it is, it goes to the model provider and stays in the session.",
            (true, true) => {
                "Sent as it is, it goes to the model provider and stays in the session. Saved, the agent gets a one-use handle in its place."
            }
            (true, false) => {
                "Sent as it is, it goes to the model provider and stays in the session. Saved, the agent gets the path of a private file in its place."
            }
        };
        let error = notice
            .error
            .clone()
            .map(|e| div().text_color(theme::error()).child(e).into_any_element());
        Some(
            div()
                .id("composer-secret-notice")
                .flex()
                .flex_col()
                .gap(crate::ui::sp(8.))
                .mx(px(8.))
                .mt(px(8.))
                .px(crate::ui::sp(10.))
                .py(crate::ui::sp(8.))
                .rounded(crate::ui::rad(ui::RADIUS))
                .bg(Hsla::from(theme::attention()).opacity(0.10))
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_muted())
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap(crate::ui::sp(8.))
                        .child(icon("alert-circle", 14.).text_color(theme::attention()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(crate::ui::sp(2.))
                                .child(div().text_color(theme::content()).child(what))
                                .child(then)
                                .children(error),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(crate::ui::sp(6.))
                        .child(
                            ui::button(
                                "composer-secret-cancel",
                                "Cancel",
                                ui::ButtonKind::Ghost,
                                true,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.close_secret_notice(cx))),
                        )
                        .child(
                            ui::button(
                                "composer-secret-send",
                                "Send anyway",
                                ui::ButtonKind::Outline,
                                true,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.send_anyway(cx))),
                        )
                        .when(can_save, |d| {
                            d.child(
                                ui::button(
                                    "composer-secret-save",
                                    "Save as a secret",
                                    ui::ButtonKind::Primary,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.save_secrets(cx))),
                            )
                        }),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ghp_R4nd0mT0k3nV4lu3F0rT3st1ng0nly1234";

    fn draft(text: &str) -> EnvDraft {
        let mut d = EnvDraft::default();
        d.text.set(text);
        d
    }

    #[test]
    fn finds_secrets_in_the_text_and_the_pastes() {
        let mut d = draft(&format!("deploy with {KEY} please"));
        d.pastes.push(paste::Paste::new(
            "line
DB_PASSWORD=hunter2hunter2
"
            .into(),
        ));
        let found = find(&d, |_| false);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].0, Spot::Text);
        assert_eq!(&d.text[found[0].1.clone()], KEY);
        assert_eq!(found[1].0, Spot::Paste(0));
        assert_eq!(&d.pastes[0].text[found[1].1.clone()], "hunter2hunter2");

        // What the session was told yes to is not found again.
        assert!(find(&d, |v| v == KEY || v == "hunter2hunter2").is_empty());
        assert!(find(&draft("fix the tests"), |_| false).is_empty());
    }

    #[test]
    fn the_mask_keeps_every_offset() {
        let text = format!("é {KEY} ü");
        let found = find(&draft(&text), |_| false);
        let ranges: Vec<_> = found.into_iter().map(|(_, r)| r).collect();
        let masked = mask(&text, &ranges);
        assert_eq!(masked.len(), text.len());
        assert!(!masked.contains(KEY));
        assert!(masked.starts_with("é ***"));
        assert!(masked.ends_with("* ü"));
        assert_eq!(Composer::shown_text(&text, &[]), text);
    }

    #[test]
    fn saving_puts_the_handles_in_place_of_the_values() {
        let mut d = draft(&format!("use {KEY} and TOKEN={KEY}x"));
        d.pastes
            .push(paste::Paste::new(format!("one\nAPI_KEY={KEY}\n")));
        let found = find(&d, |_| false);
        assert_eq!(found.len(), 3, "{found:?}");
        let with: Vec<String> = (1..=3).map(|i| format!("secret-ref:{i}")).collect();
        replace(&mut d, &found, &with);
        assert_eq!(&*d.text, "use secret-ref:1 and TOKEN=secret-ref:2");
        assert_eq!(d.pastes[0].text, "one\nAPI_KEY=secret-ref:3\n");
        assert_eq!(d.pastes[0].lines, 2);
        let title = crate::agent::attach::title(&d.outgoing(Path::new("/p")), 80);
        assert!(!title.contains(KEY), "{title}");
    }

    #[test]
    fn what_a_new_session_allowed_goes_with_it() {
        let mut allowed = Allowed::default();
        let mut frame = Frame {
            env: "/p".into(),
            ..Frame::default()
        };
        allowed.allow(&frame, [KEY]);
        assert!(allowed.has(&frame, KEY));
        frame.session = Some("s1".into());
        allowed.adopt(&frame);
        assert!(allowed.has(&frame, KEY));
        frame.session = Some("s2".into());
        assert!(!allowed.has(&frame, KEY), "another session asks again");
        frame.session = None;
        assert!(!allowed.has(&frame, KEY), "the next new session asks again");
    }
}
