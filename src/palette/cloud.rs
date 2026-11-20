//! "Send to the cloud agent…": work for arakun-pilot typed in the
//! palette ("cloud: fix the tab's title"). It goes to the user's
//! open pull request picked in the GitHub tab, or to a new branch named
//! after the text. The request itself lives in `crate::github::pilot`.

use crate::github::pilot::{self, TaskRequest, TaskTarget};

/// The word that starts a task, lower case.
const LEAD: &str = "cloud:";

/// The longest branch name suggested; the user can still type a longer one.
const SLUG_MAX: usize = 40;

/// What the palette knows of arakun-pilot when it opens: there only when
/// its `client.json` exists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PilotSeed {
    /// The user's open pull request picked in the GitHub tab: (number, title).
    pub pull: Option<(u64, String)>,
    /// Whether `client.json` has the `admin_token` tasks need.
    pub admin: bool,
}

/// The task in `q` ("cloud: <task>"), possibly empty: the card shows from the
/// lead on, so it can say what to type.
pub fn parse(q: &str) -> Option<&str> {
    let t = q.trim_start();
    let head = t.get(..LEAD.len())?;
    head.eq_ignore_ascii_case(LEAD)
        .then(|| t[LEAD.len()..].trim())
}

/// The task the card is for: only when arakun-pilot is set up.
pub fn card_task<'a>(q: &'a str, seed: Option<&PilotSeed>) -> Option<&'a str> {
    seed.and(parse(q))
}

/// A short title for the task: its first line, cut at a word near 72
/// characters.
pub fn title(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or_default().trim();
    if line.chars().count() <= 72 {
        return line.to_string();
    }
    let cut: String = line.chars().take(71).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > 30 => &cut[..i],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end())
}

/// `text` cut to `max` characters, with "…" when it was longer.
pub fn short(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// A branch name from the task: `pilot/` and its first words, without
/// accents, in kebab case.
pub fn suggest_branch(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        let c = match c {
            'á' | 'à' | 'ä' | 'â' => 'a',
            'é' | 'è' | 'ë' | 'ê' => 'e',
            'í' | 'ì' | 'ï' | 'î' => 'i',
            'ó' | 'ò' | 'ö' | 'ô' => 'o',
            'ú' | 'ù' | 'ü' | 'û' => 'u',
            'ñ' => 'n',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let mut out = String::new();
    for word in slug.split('-').filter(|w| !w.is_empty()) {
        if !out.is_empty() && out.len() + 1 + word.len() > SLUG_MAX {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(word);
    }
    out.truncate(SLUG_MAX);
    if out.is_empty() {
        "pilot/task".into()
    } else {
        format!("pilot/{out}")
    }
}

/// Why git would refuse `name` as a new branch; None when it
/// takes it. The common cases of `git check-ref-format`, enough to catch a
/// typo before it reaches the Worker.
pub fn branch_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("Give the branch a name.");
    }
    let bad_char = name
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c));
    let bad = bad_char
        || name.contains("..")
        || name.contains("//")
        || name.contains("@{")
        || name == "@"
        || name.starts_with(['-', '/', '.'])
        || name.ends_with(['/', '.'])
        || name.ends_with(".lock")
        || name.split('/').any(|p| p.starts_with('.'));
    if bad {
        return Some("That branch name won't work: use letters, numbers, - and /.");
    }
    (name == "main").then_some("The task can't go straight to main.")
}

/// The request for `task`, or why it can't go yet. A task goes to the
/// picked pull request when `to_pull` and there is one, else to `branch`
/// off main.
pub fn request(
    task: &str,
    seed: &PilotSeed,
    to_pull: bool,
    branch: &str,
) -> Result<TaskRequest, String> {
    let prompt = task.trim();
    if prompt.is_empty() {
        return Err("Write what the agent should do after “cloud:”.".into());
    }
    if !seed.admin {
        return Err(pilot::NO_ADMIN.into());
    }
    let target = match seed.pull.as_ref().filter(|_| to_pull) {
        Some((n, _)) => TaskTarget::Pull(*n),
        None => {
            let name = branch.trim();
            if let Some(why) = branch_problem(name) {
                return Err(why.into());
            }
            TaskTarget::Branch {
                name: name.to_string(),
                base: "main".into(),
            }
        }
    };
    Ok(TaskRequest {
        title: title(prompt),
        prompt: prompt.to_string(),
        target,
    })
}

/// The footer's line once the Worker took a task.
pub fn sent_note(id: &str) -> String {
    let id = id.trim();
    if id.is_empty() {
        "Sent to the cloud agent. The result comes back as a card in GitHub.".into()
    } else {
        format!("Sent to the cloud agent ({id}). The result comes back as a card in GitHub.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_starts_with_its_lead() {
        assert_eq!(parse("cloud: fix the title"), Some("fix the title"));
        assert_eq!(parse("Cloud:fix"), Some("fix"));
        assert_eq!(parse("cloud"), None);
        assert_eq!(parse("CLOUD: bump the version"), Some("bump the version"));
        // The lead alone shows the card, which says what to type.
        assert_eq!(parse("cloud:"), Some(""));
        assert_eq!(parse("the cloud: something"), None);
        assert_eq!(parse("what is the cloud?"), None);
    }

    #[test]
    fn the_card_shows_only_with_pilot_set_up() {
        let seed = PilotSeed::default();
        assert_eq!(card_task("cloud: x", Some(&seed)), Some("x"));
        assert_eq!(card_task("cloud: x", None), None);
        assert_eq!(card_task("fix x", Some(&seed)), None);
    }

    #[test]
    fn titles_are_one_short_line() {
        assert_eq!(title("  fix the tab\nmore detail"), "fix the tab");
        let long = "make the palette remember what you opened at this hour and put it above everything else";
        let t = title(long);
        assert!(t.ends_with('…') && t.chars().count() <= 72, "{t}");
        assert!(long.starts_with(t.trim_end_matches('…')));
        assert_eq!(short("feat(github): add a tab", 12), "feat(github…");
        assert_eq!(short(" short ", 12), "short");
    }

    #[test]
    fn branches_are_named_after_the_task() {
        assert_eq!(
            suggest_branch("Fix the résumé page for the café"),
            "pilot/fix-the-resume-page-for-the-cafe"
        );
        assert_eq!(suggest_branch("?! … !?"), "pilot/task");
        let long = suggest_branch(
            "add an option so that github notifications make a different sound from the rest",
        );
        assert!(long.len() <= "pilot/".len() + SLUG_MAX, "{long}");
        assert!(!long.ends_with('-'));
        assert_eq!(branch_problem(&long), None);
    }

    #[test]
    fn the_request_goes_to_the_pull_or_a_new_branch() {
        let seed = PilotSeed {
            pull: Some((112, "feat: x".into())),
            admin: true,
        };
        let r = request("  fix the tab  ", &seed, true, "pilot/fix-the-tab").unwrap();
        assert_eq!(r.target, TaskTarget::Pull(112));
        assert_eq!(
            (r.title.as_str(), r.prompt.as_str()),
            ("fix the tab", "fix the tab")
        );
        // Switched to a new branch, or no pull request picked.
        let r = request("fix", &seed, false, " pilot/fix ").unwrap();
        assert_eq!(
            r.target,
            TaskTarget::Branch {
                name: "pilot/fix".into(),
                base: "main".into()
            }
        );
        let alone = PilotSeed {
            pull: None,
            admin: true,
        };
        assert!(matches!(
            request("fix", &alone, true, "pilot/a").unwrap().target,
            TaskTarget::Branch { .. }
        ));
        // What stops it.
        assert!(request("  ", &seed, true, "x").is_err());
        assert!(request("fix", &alone, true, "bad name").is_err());
        let no_admin = PilotSeed {
            admin: false,
            ..seed
        };
        assert_eq!(
            request("fix", &no_admin, true, "x").unwrap_err(),
            pilot::NO_ADMIN
        );
        assert!(sent_note("t_42").contains("(t_42)"));
        assert!(!sent_note("").contains("()"));
    }

    #[test]
    fn bad_branch_names_are_caught() {
        assert_eq!(branch_problem("pilot/fix-tab"), None);
        assert_eq!(branch_problem("fix/a.b"), None);
        for bad in [
            "",
            "with space",
            "a..b",
            "-x",
            "x/",
            "x.lock",
            "a//b",
            "x:y",
            "a/.b",
            "main",
        ] {
            assert!(branch_problem(bad).is_some(), "{bad}");
        }
    }
}
