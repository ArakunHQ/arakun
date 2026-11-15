//! Plan limits as they fill up: a desktop notification when a window of
//! Claude's, Droid's or OpenCode Go's plan crosses 80% and again at 100%, once per window
//! and reset, and the composer's warning when the picked agent is out.
//!
//! The first check after start only notes where each window is, so opening
//! arakun with a window already full doesn't notify again. Providers hidden
//! from the rail's usage card stay quiet: hiding is how one says "I know".

use super::island::Open;
use super::*;
use crate::account::UsageMode;
use crate::agent::AgentKind;
use crate::agent::opencode_plan;
use crate::agent::plan::{self, PlanUsage};
use crate::island::AlertKind;

/// One window of a plan, as the alerts see it.
struct Reading {
    /// Stable for the window's current cycle, so a reset starts over.
    key: String,
    who: &'static str,
    label: String,
    pct: f32,
    reset: Option<String>,
}

/// 0 below 80%, 1 from 80%, 2 at 100%.
fn level(pct: f32) -> u8 {
    if pct >= 100. {
        2
    } else if pct >= 80. {
        1
    } else {
        0
    }
}

/// Claude's windows, or OpenCode Go's (`who` names which).
fn plan_readings(plan: &PlanUsage, who: &'static str, now: u64) -> Vec<Reading> {
    plan.windows
        .iter()
        .filter(|w| w.active)
        .map(|w| Reading {
            key: format!("{who}:{}:{}", w.label, w.resets_at.unwrap_or(0)),
            who,
            label: w.label.clone(),
            pct: w.percent as f32,
            reset: w.reset_text(now),
        })
        .collect()
}

fn droid_readings(account: &Account, now: u64) -> Vec<Reading> {
    let Some(limits) = account.limits() else {
        return Vec::new();
    };
    let pools = [
        ("Standard", limits.standard.as_ref()),
        ("Core", limits.core.as_ref()),
    ];
    pools
        .into_iter()
        .filter_map(|(name, pool)| Some((name, pool?)))
        .flat_map(|(name, pool)| {
            pool.buckets()
                .into_iter()
                .filter(|(_, b)| b.active(now))
                .map(move |(label, b)| Reading {
                    key: format!("droid:{name}:{label}:{}", b.window_end.unwrap_or(0)),
                    who: "Droid",
                    label: format!("{name} · {label}"),
                    pct: b.current(now),
                    reset: Some(account_view::reset_label(&b, now)),
                })
        })
        .collect()
}

/// The notifications `readings` call for, given the level each window last
/// notified at (updated here). `first` only records the levels.
fn alerts(
    readings: Vec<Reading>,
    seen: &mut std::collections::HashMap<String, u8>,
    first: bool,
) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    for r in readings {
        let lvl = level(r.pct);
        let before = seen.insert(r.key.clone(), lvl).unwrap_or(0);
        if first || lvl <= before {
            continue;
        }
        let reset = r.reset.map(|t| format!(" · {t}")).unwrap_or_default();
        out.push(if lvl == 2 {
            (
                format!("{} reached a limit", r.who),
                format!("{} is used up{reset}", r.label),
                true,
            )
        } else {
            (
                format!("{} is at {:.0}%", r.who, r.pct),
                format!("{} is filling up{reset}", r.label),
                false,
            )
        });
    }
    out
}

impl Shell {
    /// Notifies about windows that crossed 80% or 100% since the last check.
    pub(super) fn check_limit_alerts(&mut self, cx: &mut Context<Self>) {
        let now = crate::account::unix_now();
        let mut readings = Vec::new();
        if account_view::rail_shows(AgentKind::Claude) {
            if let Some(plan) = plan::latest() {
                readings.extend(plan_readings(&plan, "Claude Code", now));
            }
            // The borrowed seat fills up too, and it is the one to wait on while
            // the own one is in use: alert for it under its own name.
            #[cfg(feature = "coderlabs")]
            if let Some(borrowed) = plan::latest_seats().and_then(|s| s.borrowed)
                && !plan::active_is_borrowed()
            {
                readings.extend(plan_readings(&borrowed, "Borrowed Claude seat", now));
            }
        }
        if account_view::rail_shows(AgentKind::Droid) {
            readings.extend(droid_readings(self.account.read(cx), now));
        }
        if account_view::rail_shows(AgentKind::OpenCode)
            && let Some(go) = opencode_plan::latest()
        {
            readings.extend(plan_readings(&go, "OpenCode Go", now));
        }
        let first = !self.limits_seen;
        self.limits_seen = true;
        for (title, body, critical) in alerts(readings, &mut self.limit_alerts, first) {
            let kind = if critical {
                AlertKind::Failure
            } else {
                AlertKind::Warning
            };
            self.notify(kind, &title, &body, Some(Open::Usage), cx);
        }
        cx.notify();
    }

    /// Why the composer's agent can't take a prompt right now, if it can't.
    pub(super) fn agent_out_of_limits(&self, agent: model::Agent, cx: &App) -> Option<String> {
        let now = crate::account::unix_now();
        match agent {
            model::Agent::Claude => {
                let plan = plan::latest()?;
                let full = plan.windows.iter().find(|w| w.active && w.percent >= 100)?;
                let reset = full
                    .reset_text(now)
                    .map(|t| format!(", {t}"))
                    .unwrap_or_default();
                Some(format!(
                    "Claude Code's {} is used up{reset}.",
                    full.label.to_lowercase()
                ))
            }
            model::Agent::Droid => {
                let account = self.account.read(cx);
                let limits = account.limits()?;
                (limits.mode(now) == UsageMode::Blocked)
                    .then(|| format!("Droid's plan is out: {}", limits.summary(now)))
            }
            // A full Go plan doesn't stop OpenCode: other providers, or Go's
            // extra usage, may still take the prompt; the provider says so if not.
            model::Agent::OpenCode | model::Agent::WorkersAi => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Reading, alerts, level};
    use std::collections::HashMap;

    fn r(key: &str, pct: f32) -> Reading {
        Reading {
            key: key.into(),
            who: "Droid",
            label: "Core · Week".into(),
            pct,
            reset: Some("resets Oct 31".into()),
        }
    }

    #[test]
    fn levels() {
        assert_eq!(
            [level(10.), level(80.), level(99.9), level(100.)],
            [0, 1, 1, 2]
        );
    }

    #[test]
    fn each_crossing_notifies_once_per_cycle() {
        let mut seen = HashMap::new();
        // Opening arakun with a full window says nothing.
        assert!(alerts(vec![r("w:1", 100.)], &mut seen, true).is_empty());
        assert!(alerts(vec![r("w:1", 100.)], &mut seen, false).is_empty());
        // Another window filling up: 80% once, then 100% once.
        assert!(alerts(vec![r("x:1", 50.)], &mut seen, false).is_empty());
        let a = alerts(vec![r("x:1", 85.)], &mut seen, false);
        assert_eq!(a.len(), 1);
        assert!(!a[0].2 && a[0].0.contains("85%"));
        assert!(alerts(vec![r("x:1", 90.)], &mut seen, false).is_empty());
        let b = alerts(vec![r("x:1", 100.)], &mut seen, false);
        assert!(b[0].2 && b[0].1.contains("used up · resets Oct 31"));
        // A reset is a new key: it can notify again.
        assert_eq!(alerts(vec![r("x:2", 81.)], &mut seen, false).len(), 1);
    }
}
