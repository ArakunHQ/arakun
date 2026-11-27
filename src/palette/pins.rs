//! What the person pinned from the palette (Alt+P): shown at the top of the
//! palette and of the sidebar, opened with Ctrl+Alt+1..9. Kept in `pins.json`
//! next to the state.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One pinned thing: what kind it is, its id within that kind, and how it reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pin {
    pub kind: String,
    pub id: String,
    pub label: String,
}

impl Pin {
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind, self.id)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pins {
    #[serde(default)]
    pub items: Vec<Pin>,
}

/// Ctrl+Alt+1..9 reach this many.
pub const MAX: usize = 9;

impl Pins {
    pub fn has(&self, key: &str) -> bool {
        self.items.iter().any(|p| p.key() == key)
    }

    /// Pins it at the end, or unpins it when already pinned. Returns whether
    /// it is pinned now. Past `MAX` nothing new is added.
    pub fn toggle(&mut self, pin: Pin) -> bool {
        let key = pin.key();
        if let Some(i) = self.items.iter().position(|p| p.key() == key) {
            self.items.remove(i);
            return false;
        }
        if self.items.len() >= MAX {
            return false;
        }
        self.items.push(pin);
        true
    }

    /// Moves the pin at `from` one step (`-1` up, `1` down).
    pub fn nudge(&mut self, key: &str, delta: isize) -> bool {
        let Some(i) = self.items.iter().position(|p| p.key() == key) else {
            return false;
        };
        let j = i as isize + delta;
        if j < 0 || j as usize >= self.items.len() {
            return false;
        }
        self.items.swap(i, j as usize);
        true
    }

    pub fn load() -> Self {
        let mut pins = Self::read(&file());
        pins.migrate();
        pins
    }

    /// Pins of rows that are gone open what took their place. A view's id is
    /// its label, and Extensions is now Marketplace. The Marketplace and
    /// Automations commands were the same as their views, and the layout
    /// switches the same as the toggle commands.
    fn migrate(&mut self) {
        // From kind and id, to kind, id and label.
        const MOVED: &[[&str; 5]] = &[
            ["view", "extensions", "view", "marketplace", "Marketplace"],
            [
                "command",
                "extensions",
                "view",
                "marketplace",
                "Marketplace",
            ],
            [
                "command",
                "automations",
                "view",
                "automations",
                "Automations",
            ],
            [
                "setting",
                "dock",
                "command",
                "toggle-dock",
                "Toggle terminal dock",
            ],
            [
                "setting",
                "sidebar",
                "command",
                "toggle-sidebar",
                "Toggle sidebar",
            ],
            [
                "setting",
                "panel",
                "command",
                "toggle-panel",
                "Toggle workspace panel",
            ],
            [
                "setting",
                "subagents",
                "command",
                "toggle-subagents",
                "Toggle session tab",
            ],
        ];
        for pin in &mut self.items {
            if let Some([_, _, kind, id, label]) =
                MOVED.iter().find(|m| pin.kind == m[0] && pin.id == m[1])
            {
                pin.kind = (*kind).into();
                pin.id = (*id).into();
                pin.label = (*label).into();
            }
        }
        // Two old pins may now be the same one.
        let mut seen = std::collections::HashSet::new();
        self.items.retain(|p| seen.insert(p.key()));
    }

    pub fn save(&self) {
        self.write(&file());
    }

    fn read(path: &std::path::Path) -> Self {
        crate::stored::load(path).unwrap_or_default()
    }

    fn write(&self, path: &std::path::Path) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            crate::stored::write_soon(path, bytes, "could not save the pins");
        }
    }
}

fn file() -> PathBuf {
    crate::paths::store(crate::paths::PINS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(id: &str) -> Pin {
        Pin {
            kind: "view".into(),
            id: id.into(),
            label: id.into(),
        }
    }

    #[test]
    fn an_extensions_pin_opens_the_marketplace() {
        let mut p = Pins::default();
        p.toggle(Pin {
            kind: "view".into(),
            id: "extensions".into(),
            label: "Extensions".into(),
        });
        p.toggle(pin("github"));
        p.migrate();
        assert!(p.has("view:marketplace"));
        assert!(!p.has("view:extensions"));
        assert_eq!(p.items[0].label, "Marketplace");
        assert!(p.has("view:github"), "other pins stay as they were");
    }

    #[test]
    fn pins_of_removed_rows_open_what_replaced_them() {
        let mut p = Pins::default();
        for (kind, id) in [
            ("command", "extensions"),
            ("view", "marketplace"),
            ("setting", "dock"),
            ("setting", "redacted"),
        ] {
            p.toggle(Pin {
                kind: kind.into(),
                id: id.into(),
                label: id.into(),
            });
        }
        p.migrate();
        let keys: Vec<String> = p.items.iter().map(Pin::key).collect();
        assert_eq!(
            keys,
            [
                "view:marketplace",
                "command:toggle-dock",
                "setting:redacted"
            ]
        );
    }

    #[test]
    fn toggling_pins_and_unpins() {
        let mut p = Pins::default();
        assert!(p.toggle(pin("github")));
        assert!(p.has("view:github"));
        assert!(!p.toggle(pin("github")));
        assert!(!p.has("view:github"));
    }

    #[test]
    fn no_more_than_nine() {
        let mut p = Pins::default();
        for i in 0..12 {
            p.toggle(pin(&i.to_string()));
        }
        assert_eq!(p.items.len(), MAX);
    }

    #[test]
    fn reordering() {
        let mut p = Pins::default();
        for id in ["a", "b", "c"] {
            p.toggle(pin(id));
        }
        assert!(p.nudge("view:c", -1));
        assert_eq!(
            p.items.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            ["a", "c", "b"]
        );
        assert!(!p.nudge("view:a", -1));
    }

    #[test]
    fn pins_persist() {
        let dir = std::env::temp_dir().join(format!("arakun-pins-{}", std::process::id()));
        let path = dir.join("pins.json");
        let mut p = Pins::default();
        p.toggle(pin("notes"));
        p.write(&path);
        assert_eq!(Pins::read(&path), p);
        // On disk before the file is broken by hand below.
        crate::durable::flush();
        // A missing or broken file reads as no pins, and a broken one is
        // kept beside the next save.
        std::fs::write(&path, b"{nope").unwrap();
        assert_eq!(Pins::read(&path), Pins::default());
        p.write(&path);
        assert_eq!(Pins::read(&path), p);
        assert_eq!(crate::stored::tests::kept(&path).as_deref(), Some("{nope"));
        crate::durable::flush();
        let _ = std::fs::remove_dir_all(dir);
    }
}
