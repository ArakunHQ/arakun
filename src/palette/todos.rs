//! The person's to-dos, added from the palette ("to-do: call Dani",
//! "agrega a mis pendientes revisar el PR") and ticked off there with Enter.
//! Kept in `todos.json` next to the state. A to-do with a time is a reminder
//! instead (see `rules`).

use std::path::PathBuf;

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
        std::fs::read(file())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = crate::durable::write(&file(), &bytes);
        }
    }
}

fn file() -> PathBuf {
    super::store_path("todos.json")
}

/// What to add when a sentence asks for a to-do: "to-do: call Dani",
/// "agrega a mis pendientes revisar el PR", "add buy milk to my todos".
pub fn parse(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    // Offsets below come from `lower`. They fit the typed text only when no
    // letter changed its length when lowered (the Kelvin sign shrinks, `İ`
    // grows); else cut the lower-case text, which never panics.
    let t = if super::rank::lower_keeps_offsets(trimmed) {
        trimmed
    } else {
        lower.as_str()
    };
    const HEADS: &[&str] = &[
        "agrega a mis pendientes",
        "agregar a mis pendientes",
        "añade a mis pendientes",
        "anade a mis pendientes",
        "agrega a pendientes",
        "añade a pendientes",
        "agrega a mi lista",
        "añade a mi lista",
        "crea un to-do de",
        "crea un to-do para",
        "crea un to-do",
        "créame un to-do de",
        "creame un to-do de",
        "créame un to-do",
        "creame un to-do",
        "crea un pendiente de",
        "crea un pendiente",
        "nuevo pendiente",
        "pendiente:",
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
            let what = t[t.len() - rest.len()..]
                .trim()
                .trim_start_matches([':', '-'])
                .trim();
            return (!what.is_empty()).then(|| what.to_string());
        }
    }
    // "add buy milk to my todos" / "agrega comprar leche a mis pendientes"
    for (head, tails) in [
        (
            "add ",
            &[
                "to my todos",
                "to my to-dos",
                "to my todo list",
                "to my list",
            ][..],
        ),
        (
            "agrega ",
            &["a mis pendientes", "a mi lista", "a pendientes"][..],
        ),
        (
            "añade ",
            &["a mis pendientes", "a mi lista", "a pendientes"][..],
        ),
    ] {
        if lower.starts_with(head) {
            for tail in tails {
                if let Some(mid) = lower.strip_suffix(tail)
                    && mid.ends_with(char::is_whitespace)
                {
                    let what = t[head.len()..mid.len()].trim();
                    if !what.is_empty() {
                        return Some(what.to_string());
                    }
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
        assert_eq!(
            parse("to-do: llamar a Dani").as_deref(),
            Some("llamar a Dani")
        );
        assert_eq!(
            parse("Agrega a mis pendientes revisar el PR de Rik").as_deref(),
            Some("revisar el PR de Rik")
        );
        assert_eq!(
            parse("créame un to-do de pagar la luz").as_deref(),
            Some("pagar la luz")
        );
        assert_eq!(
            parse("add buy milk to my todos").as_deref(),
            Some("buy milk")
        );
        assert_eq!(
            parse("agrega comprar café a mis pendientes").as_deref(),
            Some("comprar café")
        );
        assert_eq!(parse("todo:"), None);
        assert_eq!(parse("add a todos endpoint to the API"), None);
        assert_eq!(parse("nuevo pendientes.rs en el repo"), None);
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
        assert_eq!(parse("todo: \u{212A}").as_deref(), Some("k"));
        assert_eq!(parse("todo: İstanbul").as_deref(), Some("i\u{307}stanbul"));
        assert_eq!(parse("add \u{212A}é to my todos").as_deref(), Some("ké"));
        assert_eq!(
            parse("añade İİ a mis pendientes").as_deref(),
            Some("i\u{307}i\u{307}")
        );
        // Letters that keep their length keep their case.
        assert_eq!(
            parse("TODO: Llamar a José 🦀").as_deref(),
            Some("Llamar a José 🦀")
        );
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
}
