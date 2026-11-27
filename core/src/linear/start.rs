//! The first steps for an issue delegated in Linear, before any session:
//! whether a delivery is read, kept or refused, the branch it works on, and
//! what Linear is told when it cannot start.

use std::path::{Path, PathBuf};

use super::delegation::Refusal;
use super::prompt;
use crate::phone::linear::Delivery;

/// The most characters of a reason shown in Linear: git and Linear can say a lot.
const REASON_LIMIT: usize = 300;

/// What a panic while reading the issue or making the environment shows in
/// Linear, in place of its text.
pub const HIT_AN_ERROR: &str = "Arakun hit an error.";

/// Deliveries kept while the Arakun account is not loaded yet.
pub const KEPT: usize = 50;

/// New issues read from Linear at once. A computer back after days can get
/// many deliveries together; the rest are kept until a read ends.
pub const MAX_READS: usize = 8;

/// Keeps `d` for when the account is loaded: once per delivery id, and the
/// oldest go beyond [`KEPT`].
pub fn keep(kept: &mut Vec<Delivery>, d: Delivery) {
    if kept.iter().any(|k| k.id == d.id) {
        return;
    }
    kept.push(d);
    let extra = kept.len().saturating_sub(KEPT);
    kept.drain(..extra);
}

/// What becomes of the kept deliveries when the account changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    /// Still not known whether this account may use Linear.
    Hold,
    /// The gate is open: they are handled now.
    Replay,
    /// The account may not use Linear: they will never be handled here.
    Drop,
}

pub fn kept_after(account_loaded: bool, flagged: bool, label: bool) -> Kept {
    if !account_loaded {
        Kept::Hold
    } else if !flagged {
        Kept::Drop
    } else if label {
        Kept::Replay
    } else {
        // No relay label: nothing new comes in, and nothing can be accepted.
        Kept::Hold
    }
}

/// Whether one more new issue may be read now.
pub fn may_read(reads: usize) -> bool {
    reads < MAX_READS
}

/// The second look at the guard, once the environment is made: it must
/// still say yes, and for the project the environment was made in. A
/// mapping moved to another project meanwhile no longer covers this one.
pub fn guard_again(now: Result<PathBuf, Refusal>, made_in: &Path) -> Result<(), Refusal> {
    match now {
        Ok(project) if project == made_in => Ok(()),
        Ok(_) => Err(Refusal::Unmapped),
        Err(r) => Err(r),
    }
}

/// The branch to try next when the environment could not be made on
/// `branch`: the issue's own, once, when git refused the name (a branch `a`
/// is in the way of `a/b`).
pub fn next_branch(error: &str, branch: &str, own: &str, may_retry: bool) -> Option<String> {
    (may_retry && own != branch && super::env::name_refused(error)).then(|| own.to_string())
}

/// What a delivery's first look decides, before the issue is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum First {
    /// No Arakun account, flag or relay label: the relay sends it again
    /// later. Or the same delivery is being handled now: that one accepts it
    /// when it is done.
    Ignore,
    /// Handled before: accepted again so the relay stops sending it.
    AcceptOnly,
    /// No sign-in for the delivery's Linear workspace here.
    RefuseNoSignIn,
    Read,
}

pub fn first_step(in_flight: bool, handled: bool, gate: bool, signed_in_workspace: bool) -> First {
    if !gate || in_flight {
        First::Ignore
    } else if handled {
        First::AcceptOnly
    } else if !signed_in_workspace {
        First::RefuseNoSignIn
    } else {
        First::Read
    }
}

pub fn refusal_line(r: &Refusal, computer: &str) -> String {
    match r {
        Refusal::Off => format!(
            "{computer} no longer takes work from Linear. Turn it on in Arakun's Linear tab, \
             then delegate again."
        ),
        Refusal::Unmapped => format!(
            "This issue's team and project are not mapped on {computer}. Map one in Arakun's \
             Linear tab, then delegate again."
        ),
        Refusal::ProjectGone => format!(
            "The project this was mapped to is no longer in Arakun on {computer}. Fix the \
             mapping in Arakun's Linear tab, then delegate again."
        ),
    }
}

pub fn started_line(project: &str, computer: &str, branch: &str) -> String {
    format!("Started in {project} on {computer}, on the branch {branch}.")
}

pub fn waiting_line(computer: &str, n: usize) -> String {
    format!(
        "Waiting on {computer}: {n} other sessions from Linear are running. This one starts \
         when one finishes."
    )
}

pub fn no_sign_in_line(computer: &str) -> String {
    format!(
        "{computer} is not signed in to this Linear workspace. Sign in in Arakun's Linear tab, \
         then delegate again."
    )
}

pub fn busy_line(computer: &str) -> String {
    format!(
        "Arakun is already working on this issue on {computer}. Reply in that session, or \
         stop it first."
    )
}

pub fn unreadable_line(text: &str) -> String {
    format!(
        "Arakun could not read this issue from Linear: {}",
        said(text)
    )
}

pub fn no_env_line(text: &str) -> String {
    format!(
        "Arakun could not make an environment for this: {}",
        said(text)
    )
}

pub fn failed_line(text: &str) -> String {
    format!("The session failed: {}", said(text))
}

/// [`reason`] with this computer's home folder.
fn said(text: &str) -> String {
    reason(text, std::env::home_dir().as_deref())
}

/// A reason as Linear shows it: its first line, the home folder as `~` so the
/// person's local paths stay here, no control characters, and short.
pub fn reason(text: &str, home: Option<&Path>) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    let home = home
        .and_then(Path::to_str)
        .map(|h| h.trim_end_matches(['/', '\\']))
        .filter(|h| !h.is_empty());
    let line = match home {
        Some(home) => tilde(line, home),
        None => line.to_string(),
    };
    line.chars()
        .filter(|c| !c.is_control())
        .take(REASON_LIMIT)
        .collect()
}

/// `home` as `~` wherever it is a whole folder: `/home/rik` in
/// `/home/rikki` stays.
fn tilde(line: &str, home: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find(home) {
        let after = &rest[i + home.len()..];
        let whole = after
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.')));
        out.push_str(&rest[..i]);
        out.push_str(if whole { "~" } else { home });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// The branch a delegated session works on. Never one of the project's base
/// branches: the session commits there, and a pull request needs its own branch.
pub fn work_branch(branch: &str, identifier: &str, bases: &[String]) -> String {
    if bases.iter().any(|b| b.eq_ignore_ascii_case(branch)) {
        own_branch(identifier)
    } else {
        branch.to_string()
    }
}

/// The branch for an issue when Linear's own name for it cannot be used.
pub fn own_branch(identifier: &str) -> String {
    prompt::own_branch(identifier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phone::linear::DeliveryKind;

    #[test]
    fn each_refusal_has_its_line() {
        assert_eq!(
            refusal_line(&Refusal::Off, "rik-mac"),
            "rik-mac no longer takes work from Linear. Turn it on in Arakun's Linear tab, \
             then delegate again."
        );
        assert_eq!(
            refusal_line(&Refusal::Unmapped, "rik-mac"),
            "This issue's team and project are not mapped on rik-mac. Map one in Arakun's \
             Linear tab, then delegate again."
        );
        assert_eq!(
            refusal_line(&Refusal::ProjectGone, "rik-mac"),
            "The project this was mapped to is no longer in Arakun on rik-mac. Fix the \
             mapping in Arakun's Linear tab, then delegate again."
        );
    }

    #[test]
    fn the_started_line_names_the_project_computer_and_branch() {
        assert_eq!(
            started_line("arakun", "rik-mac", "rik/eng-412-fix"),
            "Started in arakun on rik-mac, on the branch rik/eng-412-fix."
        );
    }

    #[test]
    fn the_other_lines_read_as_in_the_plan() {
        assert_eq!(
            waiting_line("rik-mac", 3),
            "Waiting on rik-mac: 3 other sessions from Linear are running. This one starts \
             when one finishes."
        );
        assert_eq!(
            no_sign_in_line("rik-mac"),
            "rik-mac is not signed in to this Linear workspace. Sign in in Arakun's Linear \
             tab, then delegate again."
        );
        assert_eq!(
            unreadable_line("Linear refused the sign-in. Sign in again."),
            "Arakun could not read this issue from Linear: Linear refused the sign-in. \
             Sign in again."
        );
        assert_eq!(
            no_env_line("git said \"fatal: bad\""),
            "Arakun could not make an environment for this: git said \"fatal: bad\""
        );
    }

    #[test]
    fn a_long_reason_is_cut_on_a_character() {
        let line = no_env_line(&"é".repeat(REASON_LIMIT + 50));
        assert_eq!(line.matches('é').count(), REASON_LIMIT);
        let line = unreadable_line(&"x".repeat(REASON_LIMIT + 1));
        assert!(line.ends_with(&format!(": {}", "x".repeat(REASON_LIMIT))));
    }

    #[test]
    fn the_first_look_goes_in_order() {
        let all = [false, true];
        // No account, flag or label: the relay sends it again later.
        for in_flight in all {
            for handled in all {
                for signed_in in all {
                    assert_eq!(
                        first_step(in_flight, handled, false, signed_in),
                        First::Ignore
                    );
                }
            }
        }
        // Being handled now: not accepted yet, the first frame does that.
        for handled in all {
            for signed_in in all {
                assert_eq!(first_step(true, handled, true, signed_in), First::Ignore);
            }
        }
        // Handled before: accepted again, whatever else holds.
        assert_eq!(first_step(false, true, true, false), First::AcceptOnly);
        assert_eq!(first_step(false, true, true, true), First::AcceptOnly);
        assert_eq!(first_step(false, false, true, false), First::RefuseNoSignIn);
        assert_eq!(first_step(false, false, true, true), First::Read);
    }

    fn delivery(id: &str) -> Delivery {
        Delivery {
            id: id.into(),
            kind: DeliveryKind::Created,
            agent_session: format!("as-{id}"),
            issue: "i1".into(),
            organization: "w1".into(),
            activity: None,
            reason: None,
            body: None,
            guidance: Vec::new(),
        }
    }

    #[test]
    fn a_delivery_before_the_account_is_kept_once() {
        let mut kept = Vec::new();
        keep(&mut kept, delivery("d1"));
        keep(&mut kept, delivery("d2"));
        // The relay sent it again: still one.
        keep(&mut kept, delivery("d1"));
        let ids: Vec<&str> = kept.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["d1", "d2"], "in the order they came");
    }

    #[test]
    fn the_oldest_kept_deliveries_go_beyond_the_limit() {
        let mut kept = Vec::new();
        for i in 0..KEPT + 2 {
            keep(&mut kept, delivery(&format!("d{i}")));
        }
        assert_eq!(kept.len(), KEPT);
        assert_eq!(kept[0].id, "d2");
        assert_eq!(kept[KEPT - 1].id, format!("d{}", KEPT + 1));
        // One that fell off is new again.
        keep(&mut kept, delivery("d0"));
        assert_eq!((kept.len(), kept[KEPT - 1].id.as_str()), (KEPT, "d0"));
    }

    #[test]
    fn kept_deliveries_go_when_the_gate_opens_or_the_flag_is_missing() {
        // Not loaded yet: whatever else holds, they wait.
        for flagged in [false, true] {
            for label in [false, true] {
                assert_eq!(kept_after(false, flagged, label), Kept::Hold);
            }
        }
        assert_eq!(kept_after(true, true, true), Kept::Replay);
        // Loaded without the flag: never handled here.
        assert_eq!(kept_after(true, false, true), Kept::Drop);
        assert_eq!(kept_after(true, false, false), Kept::Drop);
        assert_eq!(kept_after(true, true, false), Kept::Hold);
    }

    #[test]
    fn only_so_many_new_issues_are_read_at_once() {
        assert!(may_read(0));
        assert!(may_read(MAX_READS - 1));
        assert!(!may_read(MAX_READS));
        assert!(!may_read(MAX_READS + 1));
    }

    #[test]
    fn the_second_guard_wants_the_same_project() {
        let made_in = Path::new("/p/a");
        assert_eq!(guard_again(Ok(PathBuf::from("/p/a")), made_in), Ok(()));
        // The mapping moved to another project while the environment was made.
        assert_eq!(
            guard_again(Ok(PathBuf::from("/p/b")), made_in),
            Err(Refusal::Unmapped)
        );
        for r in [Refusal::Off, Refusal::Unmapped, Refusal::ProjectGone] {
            assert_eq!(guard_again(Err(r), made_in), Err(r));
        }
    }

    #[test]
    fn a_refused_branch_name_falls_back_once_to_the_issues_own() {
        let refused = "git said \"fatal: cannot lock ref 'refs/heads/a/b': 'refs/heads/a' \
                       exists; cannot create 'refs/heads/a/b'\"";
        let own = own_branch("ENG-412");
        assert_eq!(own, "linear/eng-412");
        assert_eq!(
            next_branch(refused, "a/b", &own, true).as_deref(),
            Some("linear/eng-412")
        );
        // Once: the second try's failure is reported.
        assert_eq!(next_branch(refused, &own, &own, true), None);
        assert_eq!(next_branch(refused, "a/b", &own, false), None);
        // Any other failure would fail on the other name too.
        assert_eq!(
            next_branch("the project has no commits yet", "a/b", &own, true),
            None
        );
    }

    #[test]
    fn an_issue_in_work_has_its_line() {
        assert_eq!(
            busy_line("rik-mac"),
            "Arakun is already working on this issue on rik-mac. Reply in that session, or \
             stop it first."
        );
    }

    #[test]
    fn a_reason_keeps_local_paths_and_extra_lines_out() {
        let home = Some(Path::new("/home/rik"));
        assert_eq!(
            reason(
                "fatal: '/home/rik/arakun-workspaces/app/eng-7' already exists\nhint: more",
                home
            ),
            "fatal: '~/arakun-workspaces/app/eng-7' already exists"
        );
        assert_eq!(
            reason("in /home/rik", Some(Path::new("/home/rik/"))),
            "in ~"
        );
        assert_eq!(reason("/home/rikki/x", home), "/home/rikki/x");
        assert_eq!(reason("a\tb\u{7}c", None), "abc");
        assert_eq!(reason("  \n second", home), "");
        assert_eq!(
            reason(&"é".repeat(REASON_LIMIT + 5), None).chars().count(),
            REASON_LIMIT
        );
    }

    #[test]
    fn a_session_never_works_on_a_base_branch() {
        let bases = ["main".to_string(), "develop".to_string()];
        assert_eq!(
            work_branch("rik/eng-412-fix", "ENG-412", &bases),
            "rik/eng-412-fix"
        );
        assert_eq!(work_branch("develop", "ENG-412", &bases), "linear/eng-412");
        assert_eq!(work_branch("Main", "ENG-412", &bases), "linear/eng-412");
        assert_eq!(work_branch("develop", "ENG-412", &[]), "develop");
    }
}
