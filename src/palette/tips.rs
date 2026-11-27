//! What an empty palette teaches: the scope characters, the sentences it
//! reads, and the keys that do more than open a row. Without these, nothing
//! on screen says that `>` lists commands or that "remind me…" sets a reminder.

/// One line of the tips under an empty palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tip {
    /// What to type first, shown as a key.
    pub lead: &'static str,
    pub label: &'static str,
    pub example: &'static str,
    /// What a click types into the palette. None when an example alone
    /// would make a draft for something that does not exist.
    pub fill: Option<&'static str>,
}

const fn tip(
    lead: &'static str,
    label: &'static str,
    example: &'static str,
    fill: Option<&'static str>,
) -> Tip {
    Tip {
        lead,
        label,
        example,
        fill,
    }
}

/// The tips, in the order shown. `cloud` is whether `cloud:` can send work,
/// which needs arakun-pilot set up.
pub fn tips(cloud: bool) -> Vec<Tip> {
    let mut t = vec![
        tip(">", "Commands", "> new terminal", Some(">")),
        tip("/", "Files and symbols", "/ main.rs", Some("/")),
        tip(":", "Settings", ": glass", Some(":")),
        tip("@", "People", "@ dani", Some("@")),
        tip("#", "Pull requests", "#86", Some("#")),
        tip(
            "@opus",
            "Ask a model",
            "@opus fix the build",
            Some("@opus "),
        ),
    ];
    if cloud {
        t.push(tip(
            "cloud:",
            "Send to the cloud agent",
            "cloud: fix the flaky test",
            Some("cloud: "),
        ));
    }
    t.extend([
        tip(
            "when",
            "Automations",
            "merge #64 when the checks pass",
            None,
        ),
        tip(
            "remind",
            "Reminders and timers",
            "remind me in 20 min to stretch",
            Some("remind me in 20 min to stretch"),
        ),
        tip("to-do:", "To-dos", "to-do: call Dani", Some("to-do: ")),
    ]);
    t
}

/// Keys that act on what is typed or on a row, beyond Enter.
pub const KEYS: &[(&[&str], &str)] = &[
    (&["Ctrl", "↵"], "ask an agent"),
    (&["→"], "a row's actions"),
    (&["Space"], "select several"),
];

#[cfg(test)]
mod tests {
    use super::super::query::{self, Lead, Scope};
    use super::*;

    #[test]
    fn every_scope_has_a_tip_that_types_it() {
        let t = tips(false);
        for c in ['>', '/', ':', '@', '#'] {
            let scope = Scope::from_char(c).unwrap();
            let tip = t
                .iter()
                .find(|t| t.lead == scope.glyph())
                .unwrap_or_else(|| panic!("no tip for {c}"));
            let fill = tip.fill.unwrap();
            assert!(
                matches!(query::take(fill, false, &[]), Some((Lead::Scope(s), _)) if s == scope),
                "{fill} does not open {scope:?}"
            );
        }
    }

    #[test]
    fn the_examples_read_as_what_they_show() {
        let t = tips(true);
        let find = |label: &str| t.iter().find(|t| t.label == label).unwrap();
        assert!(matches!(
            query::take(find("Ask a model").fill.unwrap(), false, &[]),
            Some((Lead::Chip(query::Chip::Model { .. }), _))
        ));
        assert_eq!(
            super::super::cloud::parse(find("Send to the cloud agent").example),
            Some("fix the flaky test")
        );
        assert_eq!(
            super::super::todos::parse(find("To-dos").example).as_deref(),
            Some("call Dani")
        );
        assert!(
            super::super::rules::parse(find("Automations").example, Some("o/r"), &[]).is_some()
        );
        let clock = super::super::rules::Clock::system(1_800_000_000);
        assert!(
            super::super::rules::parse_reminder(find("Reminders and timers").example, &clock)
                .is_some()
        );
    }

    #[test]
    fn cloud_shows_only_when_it_can_send() {
        assert!(!tips(false).iter().any(|t| t.lead == "cloud:"));
        assert!(tips(true).iter().any(|t| t.lead == "cloud:"));
    }
}
