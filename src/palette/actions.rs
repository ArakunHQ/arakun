//! What → offers on a row: the actions of its kind, each with a letter.

use super::item::{Item, Kind, Preview, Target};

#[derive(Clone, Debug, PartialEq)]
pub enum Do {
    Go(Target),
    Pin,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub label: String,
    pub icon: &'static str,
    /// The letter that picks it while the menu is open.
    pub key: char,
    pub what: Do,
}

fn a(label: impl Into<String>, icon: &'static str, key: char, what: Do) -> Action {
    Action {
        label: label.into(),
        icon,
        key,
        what,
    }
}

/// The actions for `item`, the default (what Enter does) first. `pinned`
/// says whether it is pinned now; `read_only` hides GitHub writes.
pub fn for_item(item: &Item, pinned: bool, read_only: bool) -> Vec<Action> {
    let open = |label: &str, icon| a(label, icon, 'o', Do::Go(item.target.clone()));
    let mut out = Vec::new();
    match (&item.kind, &item.target) {
        (Kind::Run, Target::Run { root, id }) => {
            out.push(a(
                "Fix with agent",
                "sparkles",
                'f',
                Do::Go(Target::FixRun {
                    root: root.clone(),
                    id: *id,
                }),
            ));
            out.push(open("Open in Runs", "activity01"));
            if !read_only {
                out.push(a(
                    "Re-run",
                    "play",
                    'r',
                    Do::Go(Target::Rerun {
                        root: root.clone(),
                        id: *id,
                    }),
                ));
            }
            if let Preview::Run { log, .. } = &item.preview
                && !log.is_empty()
            {
                out.push(a(
                    "Copy log",
                    "copy01",
                    'c',
                    Do::Go(Target::Copy(log.join("\n"))),
                ));
            }
        }
        (Kind::Pull, Target::Pull(r)) => {
            out.push(open("Open in GitHub tab", "github"));
            out.push(a(
                "Open on github.com",
                "globe",
                'w',
                Do::Go(Target::Url(r.url())),
            ));
            out.push(a("Copy link", "link01", 'c', Do::Go(Target::Copy(r.url()))));
            out.push(a(
                "Ask an agent to review",
                "sparkles",
                'a',
                Do::Go(Target::Ask {
                    prompt: format!(
                        "Review pull request {} ({}) and tell me what to change.",
                        r.label(),
                        item.title
                    ),
                    model: None,
                    branch: None,
                }),
            ));
            // Shown read-only too: picking it says why nothing is sent.
            if r.kind == crate::github::pr::ItemKind::Pull {
                out.push(a(
                    if read_only {
                        "Merge… (read-only)"
                    } else {
                        "Merge…"
                    },
                    "git-merge",
                    'm',
                    Do::Go(Target::Merge(r.clone())),
                ));
            }
        }
        (Kind::File | Kind::Symbol, Target::File { path, line }) => {
            out.push(open(
                if line.is_some() {
                    "Open at line"
                } else {
                    "Open"
                },
                "file01",
            ));
            out.push(a(
                "Ask about it",
                "sparkles",
                'a',
                Do::Go(Target::Ask {
                    prompt: match line {
                        Some(l) => format!("Explain {}:{l} and how it is used.", path.display()),
                        None => format!("Explain what {} does.", path.display()),
                    },
                    model: None,
                    branch: None,
                }),
            ));
            out.push(a(
                "Copy path",
                "copy01",
                'c',
                Do::Go(Target::Copy(path.display().to_string())),
            ));
        }
        (Kind::Session, _) => {
            out.push(open("Open", "message01"));
            out.push(a(
                "Copy title",
                "copy01",
                'c',
                Do::Go(Target::Copy(item.title.to_string())),
            ));
        }
        (Kind::Memory, _) => {
            out.push(open("Open Memory", "bot"));
            if let Preview::Memory { body, id, .. } = &item.preview {
                out.push(a("Copy", "copy01", 'c', Do::Go(Target::Copy(body.clone()))));
                out.push(a(
                    "Ask with it",
                    "sparkles",
                    'a',
                    Do::Go(Target::Ask {
                        prompt: format!("Using memory {id} ({}), ", item.title),
                        model: None,
                        branch: None,
                    }),
                ));
            }
        }
        (Kind::Branch, Target::Branch(name)) => {
            out.push(open("Open in Branches", "git-branch"));
            out.push(a(
                "New session on this branch",
                "comment-add01",
                'n',
                Do::Go(Target::Ask {
                    prompt: String::new(),
                    model: None,
                    branch: Some(name.clone()),
                }),
            ));
            out.push(a(
                "Copy name",
                "copy01",
                'c',
                Do::Go(Target::Copy(name.clone())),
            ));
        }
        (Kind::Setting, _) => out.push(a(
            "Turn on or off",
            "preference-horizontal",
            't',
            Do::Go(item.target.clone()),
        )),
        (Kind::Card | Kind::Rule, _) => out.push(open("Run", "arrow-turn-forward")),
        _ => out.push(open("Open", "arrow-turn-forward")),
    }
    if item.kind != Kind::Card && item.kind != Kind::Rule {
        out.push(a(
            if pinned { "Unpin" } else { "Pin" },
            if pinned { "pin-off" } else { "pin" },
            'p',
            Do::Pin,
        ));
    }
    out
}

/// Whether → goes into a page of its own for this row rather than opening actions.
pub fn drills(item: &Item) -> bool {
    matches!(item.kind, Kind::Project | Kind::Person) || matches!(item.target, Target::Branches)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::pr::{ItemKind, ItemRef};

    fn pull() -> Item {
        Item::new(
            "pull:o/r#64",
            Kind::Pull,
            "fix(tools): format only arakun",
            Target::Pull(ItemRef {
                repo: "o/r".into(),
                number: 64,
                kind: ItemKind::Pull,
            }),
        )
    }

    #[test]
    fn pulls_offer_merge_and_say_when_read_only() {
        let w = for_item(&pull(), false, false);
        assert!(w.iter().any(|a| a.label == "Merge…"));
        let r = for_item(&pull(), false, true);
        assert!(r.iter().any(|a| a.label == "Merge… (read-only)"));
        assert_eq!(w[0].label, "Open in GitHub tab");
        assert_eq!(w.last().unwrap().what, Do::Pin);
    }

    #[test]
    fn letters_are_unique_per_menu() {
        let acts = for_item(&pull(), true, false);
        let mut keys: Vec<char> = acts.iter().map(|a| a.key).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), acts.len());
        assert_eq!(acts.last().unwrap().label, "Unpin");
    }

    #[test]
    fn runs_lead_with_fix() {
        let item = Item::new(
            "run:1",
            Kind::Run,
            "relay · CI #4812",
            Target::Run {
                root: "/r".into(),
                id: 1,
            },
        );
        let acts = for_item(&item, false, false);
        assert_eq!(acts[0].label, "Fix with agent");
        assert!(acts.iter().any(|a| a.label == "Re-run"));
        assert!(
            !for_item(&item, false, true)
                .iter()
                .any(|a| a.label == "Re-run")
        );
    }

    #[test]
    fn projects_and_people_drill_in() {
        let p = Item::new("project:0", Kind::Project, "app", Target::Project(0));
        assert!(drills(&p));
        assert!(!drills(&pull()));
    }
}
