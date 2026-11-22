//! The person's to-dos, added from the palette ("to-do: call Dani",
//! "add buy milk to my todos") and ticked off there with Enter.
//! Kept in `todos.json` next to the state. A to-do with a time is a reminder
//! instead (see `rules`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Todo {
    pub id: u64,
    pub text: String,
    pub created: u64,
    /// When it was ticked off; open while None.
    #[serde(default)]
    pub done: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Todos {
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub items: Vec<Todo>,
}

/// A ticked-off to-do still shows, dimmed, for this long.
const DONE_SHOWS: u64 = 86_400;

impl Todos {
    pub fn add(&mut self, text: &str, now: u64) -> u64 {
        self.next_id += 1;
        self.items.push(Todo {
            id: self.next_id,
            text: text.to_string(),
            created: now,
            done: None,
        });
        self.next_id
    }

    /// Ticks it off, or opens it again. Returns whether it is done now.
    pub fn toggle(&mut self, id: u64, now: u64) -> bool {
        let Some(t) = self.items.iter_mut().find(|t| t.id == id) else {
            return false;
        };
        t.done = match t.done {
            Some(_) => None,
            None => Some(now),
        };
        t.done.is_some()
    }

    /// Open ones first (oldest first), then the ones ticked off in the last day.
    pub fn shown(&self, now: u64) -> Vec<&Todo> {
        let mut open: Vec<&Todo> = self.items.iter().filter(|t| t.done.is_none()).collect();
        open.sort_by_key(|t| t.created);
        let mut done: Vec<&Todo> = self
            .items
            .iter()
            .filter(|t| t.done.is_some_and(|d| now.saturating_sub(d) < DONE_SHOWS))
            .collect();
        done.sort_by_key(|t| std::cmp::Reverse(t.done));
        open.extend(done);
        open
    }

    /// Forgets what was ticked off more than a week ago.
    pub fn tidy(&mut self, now: u64) {
        self.items
            .retain(|t| t.done.is_none_or(|d| now.saturating_sub(d) < 7 * 86_400));
    }

    pub fn load() -> Self {
        Self::load_from(&file())
    }

    fn load_from(path: &Path) -> Self {
        crate::stored::load(path).unwrap_or_default()
    }

    pub fn save(&self) {
        self.save_to(&file());
    }

    fn save_to(&self, path: &Path) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            crate::stored::write_soon(path, bytes, "could not save the to-dos");
        }
    }
}

fn file() -> PathBuf {
    super::store_path("todos.json")
}

/// What to add when a sentence asks for a to-do: "to-do: call Dani",
/// "add a todo review the PR", "add buy milk to my todos".
pub fn parse(text: &str) -> Option<String> {
    let trimmed = text.trim();
    // Offsets below come from the lower case, where some letters change
    // length (the Kelvin sign shrinks, `İ` grows): map them back to cut the
    // typed text, so the task keeps its case.
    let low = super::rank::Lowered::new(trimmed);
    let lower = low.text.as_str();
    const HEADS: &[&str] = &[
        "to-do:",
        "todo:",
        "to do:",
        "add a todo",
        "add a to-do",
        "new todo",
        "new to-do",
    ];
    for h in HEADS {
        // Whole words: "add a todos endpoint" is not "add a todo".
        if let Some(rest) = lower.strip_prefix(h)
            && !rest.starts_with(char::is_alphanumeric)
        {
            let what = trimmed[low.back(lower.len() - rest.len()..lower.len())]
                .trim()
                .trim_start_matches([':', '-'])
                .trim();
            return (!what.is_empty()).then(|| what.to_string());
        }
    }
    // "add buy milk to my todos"
    let head = "add ";
    if lower.starts_with(head) {
        for tail in [
            "to my todos",
            "to my to-dos",
            "to my todo list",
            "to my list",
        ] {
            if let Some(mid) = lower.strip_suffix(tail)
                && mid.ends_with(char::is_whitespace)
            {
                let what = trimmed[low.back(head.len()..mid.len())].trim();
                if !what.is_empty() {
                    return Some(what.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_dos_are_read_from_sentences() {
        assert_eq!(parse("to-do: call Dani").as_deref(), Some("call Dani"));
        assert_eq!(
            parse("New to-do: review Rik's PR").as_deref(),
            Some("review Rik's PR")
        );
        assert_eq!(
            parse("add buy milk to my todos").as_deref(),
            Some("buy milk")
        );
        assert_eq!(
            parse("Add pay the power bill to my list").as_deref(),
            Some("pay the power bill")
        );
        assert_eq!(parse("todo:"), None);
        assert_eq!(parse("add a todos endpoint to the API"), None);
        assert_eq!(parse("new todos.rs in the repo"), None);
        assert_eq!(
            parse("add a todo fix the build").as_deref(),
            Some("fix the build")
        );
        assert_eq!(parse("what is a todo list"), None);
    }

    #[test]
    fn letters_that_change_length_do_not_panic() {
        // The Kelvin sign lowers to one byte, `İ` to three: offsets taken in
        // the lower-case text would cut inside a letter of the typed one.
        // The task is cut from the typed text, so it keeps its case.
        assert_eq!(parse("todo: \u{212A}").as_deref(), Some("\u{212A}"));
        assert_eq!(parse("todo: İstanbul").as_deref(), Some("İstanbul"));
        assert_eq!(
            parse("todo: Fix İstanbul/README.md").as_deref(),
            Some("Fix İstanbul/README.md")
        );
        assert_eq!(
            parse("add \u{212A}é to my todos").as_deref(),
            Some("\u{212A}é")
        );
        assert_eq!(parse("add İİ to my todos").as_deref(), Some("İİ"));
        assert_eq!(parse("İİ todo: x"), None);
        // Letters that keep their length keep their case.
        assert_eq!(parse("TODO: Call José 🦀").as_deref(), Some("Call José 🦀"));
    }

    #[test]
    fn to_dos_tick_off_and_tidy_up() {
        let mut t = Todos::default();
        let a = t.add("a", 10);
        let b = t.add("b", 20);
        assert!(t.toggle(a, 100));
        let shown: Vec<_> = t.shown(200).iter().map(|x| x.id).collect();
        assert_eq!(shown, vec![b, a], "open first, then done");
        assert!(
            t.shown(100 + DONE_SHOWS).iter().all(|x| x.id != a),
            "done ones leave after a day"
        );
        assert!(!t.toggle(a, 300), "Enter again opens it");
        t.toggle(b, 0);
        t.tidy(8 * 86_400);
        assert!(t.items.iter().all(|x| x.id != b));
    }

    #[test]
    fn unreadable_to_dos_are_kept() {
        let dir = crate::stored::tests::scratch("todos");
        let path = dir.join("todos.json");
        let edited = r#"{"items":[{"id":1,"text":"call Dani"}]}"#;
        std::fs::write(&path, edited).unwrap();
        assert_eq!(Todos::load_from(&path), Todos::default());
        Todos::default().save_to(&path);
        assert_eq!(crate::stored::tests::kept(&path).as_deref(), Some(edited));
        crate::durable::flush();
        let _ = std::fs::remove_dir_all(dir);
    }
}
