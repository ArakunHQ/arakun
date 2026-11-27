//! Every keyboard shortcut in one table. The window's bindings, the cheat
//! sheet in Settings › Keyboard shortcuts and the README's section all come
//! from `SHORTCUTS`, so they cannot disagree.
//!
//! Keys are written the Windows and Linux way ("ctrl-shift-t");
//! `platform::keystroke` and `platform::shortcut_label` make them macOS ones.
//! A key means one thing everywhere: the palette binds no key the window
//! already uses for something else (a test checks).

use gpui::{App, KeyBinding};

use crate::{palette, platform, shell};

/// Makes the binding for one key of a row.
type Make = fn(&'static str, Option<&str>) -> KeyBinding;

/// One line of the cheat sheet, and the bindings behind it.
pub struct Shortcut {
    /// The cheat sheet's heading, and the README's.
    pub group: &'static str,
    /// What the keys do. Rows of a group with the same label show as one line.
    pub label: &'static str,
    /// Where the keys work, as a gpui key context. Empty when a view reads the
    /// keys itself (the composer, the GitHub tab, the terminal): then the row
    /// only shows them.
    pub context: &'static str,
    pub keys: &'static [&'static str],
    /// One per key, or one for all of them; none when `context` is empty.
    pub actions: &'static [Make],
}

macro_rules! act {
    ($action:expr) => {
        (|keys, context| KeyBinding::new(keys, $action, context)) as Make
    };
}

const NOT_TERMINAL: &str = "Shell && !Terminal";
const ARROWS: [&str; 4] = ["left", "right", "up", "down"];

/// A row the window binds.
const fn bound(
    group: &'static str,
    label: &'static str,
    context: &'static str,
    keys: &'static [&'static str],
    actions: &'static [Make],
) -> Shortcut {
    Shortcut {
        group,
        label,
        context,
        keys,
        actions,
    }
}

/// A row a view handles itself: the table only lists it.
const fn shown(
    group: &'static str,
    label: &'static str,
    keys: &'static [&'static str],
) -> Shortcut {
    Shortcut {
        group,
        label,
        context: "",
        keys,
        actions: &[],
    }
}

const NAVIGATE: &str = "Navigate";
const PROJECTS: &str = "Projects and sessions";
const TABS: &str = "Tabs and panes";
const TERMINAL: &str = "Terminal and browser";
const SCREEN: &str = "Zoom and screen";
const PALETTE: &str = "In the palette";
const COMPOSER: &str = "In the composer";
const GITHUB: &str = "In the GitHub tab";

/// Keys a shell needs (ctrl-w, ctrl-t, alt-arrows...) stay with the terminal
/// (`!Terminal`); on macOS `bind` makes most of them Cmd chords, which a shell
/// never sees.
pub const SHORTCUTS: &[Shortcut] = &[
    bound(
        NAVIGATE,
        "Open the palette: jump to a session, environment or command",
        NOT_TERMINAL,
        &["ctrl-k", "ctrl-p"],
        &[act!(shell::QuickSwitch)],
    ),
    bound(
        NAVIGATE,
        "Previous / next environment",
        NOT_TERMINAL,
        &["alt-up", "alt-down"],
        &[act!(shell::PrevEnvironment), act!(shell::NextEnvironment)],
    ),
    bound(
        NAVIGATE,
        "Back / forward through tabs",
        NOT_TERMINAL,
        &["alt-left", "alt-right"],
        &[act!(shell::GoBackTab), act!(shell::GoForwardTab)],
    ),
    bound(
        NAVIGATE,
        "Hide or show the sidebar",
        NOT_TERMINAL,
        &["ctrl-b"],
        &[act!(shell::ToggleSidebar)],
    ),
    bound(
        NAVIGATE,
        "Hide or show the Workspace panel",
        NOT_TERMINAL,
        &["ctrl-alt-b"],
        &[act!(shell::TogglePanel)],
    ),
    bound(
        NAVIGATE,
        "Switch the Workspace panel to its Session tab and back",
        NOT_TERMINAL,
        &["ctrl-alt-s"],
        &[act!(shell::ToggleSubagents)],
    ),
    bound(
        NAVIGATE,
        "Search the session",
        "Shell && !Terminal && !Browser",
        &["ctrl-f"],
        &[act!(shell::FindInChat)],
    ),
    bound(
        NAVIGATE,
        "Open pin 1–9, from anywhere",
        "Shell",
        &[
            "ctrl-alt-1",
            "ctrl-alt-2",
            "ctrl-alt-3",
            "ctrl-alt-4",
            "ctrl-alt-5",
            "ctrl-alt-6",
            "ctrl-alt-7",
            "ctrl-alt-8",
            "ctrl-alt-9",
        ],
        &[
            act!(palette::Pinned1),
            act!(palette::Pinned2),
            act!(palette::Pinned3),
            act!(palette::Pinned4),
            act!(palette::Pinned5),
            act!(palette::Pinned6),
            act!(palette::Pinned7),
            act!(palette::Pinned8),
            act!(palette::Pinned9),
        ],
    ),
    bound(
        NAVIGATE,
        "Settings",
        "Shell",
        &["ctrl-,"],
        &[act!(shell::OpenSettings)],
    ),
    bound(
        PROJECTS,
        "New session",
        NOT_TERMINAL,
        &["ctrl-t"],
        &[act!(shell::NewSession)],
    ),
    bound(
        PROJECTS,
        "New environment, at once on a random branch",
        "Shell",
        &["ctrl-shift-n"],
        &[act!(shell::NewEnvironment)],
    ),
    bound(
        PROJECTS,
        "New environment, asking for the branch and options first",
        NOT_TERMINAL,
        &["ctrl-alt-n"],
        &[act!(shell::NewEnvironmentWithOptions)],
    ),
    bound(
        PROJECTS,
        "Add project",
        NOT_TERMINAL,
        &["ctrl-o"],
        &[act!(shell::AddProject)],
    ),
    bound(
        TABS,
        "Close the tab",
        NOT_TERMINAL,
        &["ctrl-w"],
        &[act!(shell::CloseTab)],
    ),
    bound(
        TABS,
        "Reopen the last closed tab where it was",
        NOT_TERMINAL,
        &["ctrl-alt-t"],
        &[act!(shell::ReopenClosedTab)],
    ),
    bound(
        TABS,
        "Next / previous tab, across environments",
        "Shell",
        &["ctrl-tab", "ctrl-shift-tab", "ctrl-pagedown", "ctrl-pageup"],
        &[
            act!(shell::NextTab),
            act!(shell::PrevTab),
            act!(shell::NextTab),
            act!(shell::PrevTab),
        ],
    ),
    bound(
        TABS,
        "Go to tab 1–8, or the last tab",
        "Shell",
        &[
            "ctrl-1", "ctrl-2", "ctrl-3", "ctrl-4", "ctrl-5", "ctrl-6", "ctrl-7", "ctrl-8",
            "ctrl-9",
        ],
        &[
            act!(shell::SelectTab1),
            act!(shell::SelectTab2),
            act!(shell::SelectTab3),
            act!(shell::SelectTab4),
            act!(shell::SelectTab5),
            act!(shell::SelectTab6),
            act!(shell::SelectTab7),
            act!(shell::SelectTab8),
            act!(shell::SelectLastTab),
        ],
    ),
    bound(
        TABS,
        "Move the tab left / right",
        "Shell",
        &["ctrl-shift-pageup", "ctrl-shift-pagedown"],
        &[act!(shell::MoveTabLeft), act!(shell::MoveTabRight)],
    ),
    bound(
        TABS,
        "Pin or unpin the session's tab in the title bar",
        "Shell && !Terminal && !TextInput",
        &["alt-p"],
        &[act!(shell::TogglePin)],
    ),
    bound(
        TABS,
        "Split right / down (again to unsplit)",
        NOT_TERMINAL,
        &["ctrl-d", "ctrl-shift-d"],
        &[act!(shell::SplitRight), act!(shell::SplitDown)],
    ),
    bound(
        TABS,
        "Move the focus to the other pane",
        NOT_TERMINAL,
        &[
            "ctrl-alt-left",
            "ctrl-alt-right",
            "ctrl-alt-up",
            "ctrl-alt-down",
        ],
        &[act!(shell::FocusOtherPane)],
    ),
    bound(
        TABS,
        "Swap the panes",
        NOT_TERMINAL,
        &[
            "ctrl-alt-shift-left",
            "ctrl-alt-shift-right",
            "ctrl-alt-shift-up",
            "ctrl-alt-shift-down",
        ],
        &[act!(shell::SwapPanes)],
    ),
    bound(
        TERMINAL,
        "Show or hide the terminal dock",
        NOT_TERMINAL,
        &["ctrl-j"],
        &[act!(shell::ToggleDock)],
    ),
    bound(
        TERMINAL,
        "Show or hide the terminal dock",
        "Shell",
        &["ctrl-`"],
        &[act!(shell::ToggleDock)],
    ),
    bound(
        TERMINAL,
        "New terminal",
        "Shell",
        &["ctrl-shift-t", "ctrl-shift-`"],
        &[act!(shell::NewTerminal)],
    ),
    shown(
        TERMINAL,
        "Find in the terminal (Enter older, ↓ newer)",
        &["ctrl-shift-f"],
    ),
    bound(
        TERMINAL,
        "New browser tab",
        "Shell",
        &["ctrl-shift-b"],
        &[act!(shell::NewBrowserTab)],
    ),
    // A browser tab keeps these for the page's own zoom.
    bound(
        SCREEN,
        "Zoom the interface in / out / back to 100%",
        "Shell && !Terminal && !Browser",
        &["ctrl-=", "ctrl-+", "ctrl--", "ctrl-0"],
        &[
            act!(shell::ZoomIn),
            act!(shell::ZoomIn),
            act!(shell::ZoomOut),
            act!(shell::ZoomReset),
        ],
    ),
    // Works from a terminal too: hiding the screen can't wait for a click elsewhere.
    bound(
        SCREEN,
        "Redacted mode: hide emails, keys and codes on screen",
        "Shell",
        &["ctrl-shift-h"],
        &[act!(shell::ToggleRedacted)],
    ),
    shown(PALETTE, "Open a row's actions, or go into it", &["right"]),
    shown(PALETTE, "Go back", &["left"]),
    bound(
        PALETTE,
        "Mark rows, after the arrows, for the bar at the bottom",
        "Palette",
        &["space"],
        &[act!(palette::MarkRow)],
    ),
    bound(
        PALETTE,
        "Pin or unpin the selected row",
        "Palette",
        &["alt-p"],
        &[act!(palette::TogglePin)],
    ),
    bound(
        PALETTE,
        "Move the selected pin up / down",
        "Palette",
        &["alt-shift-up", "alt-shift-down"],
        &[act!(palette::PinUp), act!(palette::PinDown)],
    ),
    shown(PALETTE, "Switch the filter", &["tab"]),
    shown(PALETTE, "Ask an agent", &["ctrl-enter"]),
    shown(COMPOSER, "Send, or queue while the agent works", &["enter"]),
    shown(COMPOSER, "New line", &["shift-enter"]),
    shown(COMPOSER, "Stop the turn and send now", &["ctrl-enter"]),
    shown(COMPOSER, "Stop the running agent turn", &["escape"]),
    shown(COMPOSER, "Earlier prompts, on an empty composer", &["up"]),
    shown(COMPOSER, "Save the draft as a ! prompt", &["ctrl-s"]),
    shown(GITHUB, "Next / previous item", &["j", "k", "down", "up"]),
    shown(
        GITHUB,
        "Open the item's sheet / close it",
        &["enter", "escape"],
    ),
    shown(GITHUB, "Approve", &["a"]),
    shown(GITHUB, "Comment", &["c"]),
    shown(GITHUB, "Review with an agent", &["r"]),
    shown(GITHUB, "Catch me up", &["u"]),
    shown(GITHUB, "Summarize", &["s"]),
    shown(GITHUB, "All or fewer details", &["d"]),
    shown(GITHUB, "Open on GitHub", &["o"]),
    shown(
        GITHUB,
        "On a pilot card: send ready answer 1–4",
        &["1", "2", "3", "4"],
    ),
    shown(GITHUB, "On a pilot card: Other answer…", &["e"]),
    shown(GITHUB, "On a pilot card: mark it dealt with", &["y"]),
];

/// Binds every row with a context.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys(bindings());
}

fn bindings() -> Vec<KeyBinding> {
    let mut out = Vec::new();
    for sc in SHORTCUTS.iter().filter(|sc| !sc.context.is_empty()) {
        for (i, keys) in sc.keys.iter().enumerate() {
            let make = sc.actions[if sc.actions.len() == 1 { 0 } else { i }];
            out.push(bind(keys, make, sc.context));
        }
    }
    out
}

/// A binding for this platform; see `platform::keystroke`. The Cmd chords it
/// makes on macOS work over the terminal too, all but Cmd+F, which finds in
/// the terminal there; Cmd+[ and Cmd+] leave a browser tab its own back and
/// forward.
fn bind(keys: &str, make: Make, context: &str) -> KeyBinding {
    let keys = platform::keystroke(keys);
    let context = match keys.as_str() {
        "cmd-[" | "cmd-]" => format!("{} && !Browser", context.replace(" && !Terminal", "")),
        k if k.starts_with("cmd-") && k != "cmd-f" => context.replace(" && !Terminal", ""),
        _ => context.to_string(),
    };
    make(platform::static_keys(&keys), Some(&context))
}

/// A line of the cheat sheet: rows of a group with the same label, merged.
pub struct Line {
    pub group: &'static str,
    pub label: &'static str,
    pub keys: Vec<&'static str>,
}

/// The cheat sheet, in the table's order.
pub fn sheet() -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    for sc in SHORTCUTS {
        match out
            .iter_mut()
            .find(|l| l.group == sc.group && l.label == sc.label)
        {
            Some(line) => line.keys.extend(sc.keys),
            None => out.push(Line {
                group: sc.group,
                label: sc.label,
                keys: sc.keys.to_vec(),
            }),
        }
    }
    out
}

/// The keys as the Windows and Linux UI writes them, one string per choice:
/// "Ctrl+Shift+T". Nine numbered keys read "Ctrl+1 … Ctrl+9" and the four
/// arrows "Ctrl+Alt+Arrows". `arrows` writes ↑ for Up and so on.
pub fn key_labels(keys: &[&str], arrows: bool) -> Vec<String> {
    let split: Vec<(&str, &str)> = keys.iter().map(|k| split_mods(k)).collect();
    let same_mods = |n: usize| split.len() == n && split.iter().all(|(m, _)| *m == split[0].0);
    let mods = split.first().map_or("", |(m, _)| *m);
    let digits: Vec<String> = (1..=9).map(|d| d.to_string()).collect();
    if same_mods(9)
        && split
            .iter()
            .map(|(_, k)| *k)
            .eq(digits.iter().map(String::as_str))
    {
        return vec![format!(
            "{} … {}",
            label(mods, "1", arrows),
            label(mods, "9", arrows)
        )];
    }
    if same_mods(4) && split.iter().map(|(_, k)| *k).eq(ARROWS) {
        return vec![label(mods, "arrows", arrows)];
    }
    split.iter().map(|(m, k)| label(m, k, arrows)).collect()
}

/// "ctrl-shift-t" as ("ctrl-shift-", "t"); "ctrl--" as ("ctrl-", "-").
fn split_mods(keys: &str) -> (&str, &str) {
    let mut at = 0;
    while let Some(m) = ["ctrl-", "alt-", "shift-", "cmd-"]
        .iter()
        .find(|m| keys[at..].starts_with(**m) && keys.len() > at + m.len())
    {
        at += m.len();
    }
    keys.split_at(at)
}

fn label(mods: &str, key: &str, arrows: bool) -> String {
    let mut out = String::new();
    for m in mods.split('-').filter(|m| !m.is_empty()) {
        out.push_str(match m {
            "ctrl" => "Ctrl+",
            "alt" => "Alt+",
            "shift" => "Shift+",
            _ => "Cmd+",
        });
    }
    let key: String = match (key, arrows) {
        ("up", true) => "↑".into(),
        ("down", true) => "↓".into(),
        ("left", true) => "←".into(),
        ("right", true) => "→".into(),
        ("up", false) => "Up".into(),
        ("down", false) => "Down".into(),
        ("left", false) => "Left".into(),
        ("right", false) => "Right".into(),
        ("arrows", _) => "Arrows".into(),
        ("pageup", _) => "PgUp".into(),
        ("pagedown", _) => "PgDn".into(),
        ("escape", _) => "Esc".into(),
        (k, _) => {
            let mut c = k.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        }
    };
    out.push_str(&key);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// The README's table, between its markers.
    fn readme_section() -> String {
        let mut out = String::from(README_BEGIN);
        out.push('\n');
        let mut group = "";
        for line in sheet() {
            if line.group != group {
                group = line.group;
                out.push_str(&format!(
                    "\n**{group}**\n\n| Keys | What they do |\n| -- | -- |\n"
                ));
            }
            let keys: Vec<String> = key_labels(&line.keys, false)
                .iter()
                .map(|choice| {
                    choice
                        .split(" … ")
                        .map(code)
                        .collect::<Vec<_>>()
                        .join(" … ")
                })
                .collect();
            out.push_str(&format!("| {} | {} |\n", keys.join(" / "), line.label));
        }
        out.push('\n');
        out.push_str(README_END);
        out
    }

    /// Markdown code, with room for a backtick in it.
    fn code(s: &str) -> String {
        if s.contains('`') {
            format!("`` {s} ``")
        } else {
            format!("`{s}`")
        }
    }

    const README_BEGIN: &str = "<!-- shortcuts: begin; written from src/keymap.rs -->";
    const README_END: &str = "<!-- shortcuts: end -->";

    /// A key gpui cannot read would panic at start.
    #[test]
    fn every_binding_parses() {
        let n: usize = SHORTCUTS
            .iter()
            .filter(|sc| !sc.context.is_empty())
            .map(|sc| sc.keys.len())
            .sum();
        assert_eq!(bindings().len(), n);
    }

    #[test]
    fn every_bound_row_has_an_action_per_key() {
        for sc in SHORTCUTS {
            if sc.context.is_empty() {
                assert!(sc.actions.is_empty(), "{}", sc.label);
            } else {
                assert!(
                    sc.actions.len() == 1 || sc.actions.len() == sc.keys.len(),
                    "{}",
                    sc.label
                );
            }
        }
    }

    #[test]
    fn no_key_is_bound_twice_in_one_context() {
        let mut seen = HashMap::new();
        for sc in SHORTCUTS.iter().filter(|sc| !sc.context.is_empty()) {
            for k in sc.keys {
                if let Some(other) = seen.insert((*k, sc.context), sc.label) {
                    assert_eq!(other, sc.label, "{k} in {}", sc.context);
                }
            }
        }
    }

    /// The palette sits in the window: a key it binds that the window binds
    /// too must not do something else there.
    #[test]
    fn the_palette_gives_no_window_key_another_meaning() {
        let window: HashMap<&str, &str> = SHORTCUTS
            .iter()
            .filter(|sc| sc.context.starts_with("Shell"))
            .flat_map(|sc| sc.keys.iter().map(move |k| (*k, sc.label)))
            .collect();
        for sc in SHORTCUTS.iter().filter(|sc| sc.context == "Palette") {
            for k in sc.keys {
                let pins =
                    sc.label.contains("Pin") && window.get(k).is_some_and(|l| l.contains("Pin"));
                assert!(
                    !window.contains_key(k) || pins,
                    "{k} is “{}” in the palette but “{}” in the window",
                    sc.label,
                    window[k]
                );
            }
        }
    }

    #[test]
    fn keys_read_as_the_ui_writes_them() {
        assert_eq!(key_labels(&["ctrl-shift-t"], true), ["Ctrl+Shift+T"]);
        assert_eq!(
            key_labels(&["ctrl--", "ctrl-+"], true),
            ["Ctrl+-", "Ctrl++"]
        );
        assert_eq!(key_labels(&["alt-up"], true), ["Alt+↑"]);
        assert_eq!(key_labels(&["alt-up"], false), ["Alt+Up"]);
        assert_eq!(key_labels(&["ctrl-pagedown"], false), ["Ctrl+PgDn"]);
        assert_eq!(key_labels(&["escape", "j"], false), ["Esc", "J"]);
        let tabs = &SHORTCUTS
            .iter()
            .find(|s| s.label.starts_with("Go to tab"))
            .unwrap()
            .keys;
        assert_eq!(key_labels(tabs, true), ["Ctrl+1 … Ctrl+9"]);
        assert_eq!(
            key_labels(
                &[
                    "ctrl-alt-left",
                    "ctrl-alt-right",
                    "ctrl-alt-up",
                    "ctrl-alt-down"
                ],
                true
            ),
            ["Ctrl+Alt+Arrows"]
        );
    }

    #[test]
    fn the_dock_merges_into_one_line() {
        let dock = sheet()
            .into_iter()
            .find(|l| l.label == "Show or hide the terminal dock")
            .unwrap();
        assert_eq!(dock.keys, ["ctrl-j", "ctrl-`"]);
    }

    /// Run with `ARAKUN_BLESS=1` to write the table into the README.
    #[test]
    fn the_readme_lists_every_shortcut() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
        let doc = std::fs::read_to_string(&path).unwrap();
        let start = doc
            .find(README_BEGIN)
            .expect("README.md has no shortcuts: begin marker");
        let end = doc[start..]
            .find(README_END)
            .expect("README.md has no shortcuts: end marker")
            + start
            + README_END.len();
        let want = readme_section();
        if doc[start..end] == want {
            return;
        }
        if std::env::var_os("ARAKUN_BLESS").is_some() {
            std::fs::write(&path, format!("{}{want}{}", &doc[..start], &doc[end..])).unwrap();
            return;
        }
        panic!(
            "the shortcuts in README.md are not the table in src/keymap.rs; \
             write them with `ARAKUN_BLESS=1 cargo test --no-default-features keymap`"
        );
    }
}
