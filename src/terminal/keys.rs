//! Keystroke → byte sequence, following xterm/alacritty (ported from Zed's
//! `terminal/src/mappings/keys.rs`, Linux only).
//!
//! Returns `None` for keys that produce plain text; those go through the input
//! handler instead so IME, compose and dead keys (ñ, á, ¿) keep working.

use std::borrow::Cow;

use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Mods {
    None,
    Alt,
    Ctrl,
    Shift,
    CtrlShift,
    Other,
}

impl Mods {
    fn new(ks: &Keystroke) -> Self {
        let m = &ks.modifiers;
        match (m.alt, m.control, m.shift, m.platform) {
            (false, false, false, false) => Mods::None,
            (true, false, false, false) => Mods::Alt,
            (false, true, false, false) => Mods::Ctrl,
            (false, false, true, false) => Mods::Shift,
            (false, true, true, false) => Mods::CtrlShift,
            _ => Mods::Other,
        }
    }
}

pub fn to_esc_str(ks: &Keystroke, mode: TermMode) -> Option<Cow<'static, str>> {
    let mods = Mods::new(ks);
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let key = ks.key.as_str();

    let fixed: Option<&'static str> = match (key, mods) {
        ("tab", Mods::None) => Some("\x09"),
        ("escape", Mods::None) => Some("\x1b"),
        ("enter", Mods::None) => Some("\x0d"),
        ("enter", Mods::Shift) => Some("\x0a"),
        ("enter", Mods::Alt) => Some("\x1b\x0d"),
        ("backspace", Mods::None) => Some("\x7f"),
        ("tab", Mods::Shift) => Some("\x1b[Z"),
        ("backspace", Mods::Ctrl) => Some("\x08"),
        ("backspace", Mods::Alt) => Some("\x1b\x7f"),
        ("backspace", Mods::Shift) => Some("\x7f"),
        ("space", Mods::Ctrl) => Some("\x00"),
        ("home", Mods::None) => Some(if app_cursor { "\x1bOH" } else { "\x1b[H" }),
        ("end", Mods::None) => Some(if app_cursor { "\x1bOF" } else { "\x1b[F" }),
        ("up", Mods::None) => Some(if app_cursor { "\x1bOA" } else { "\x1b[A" }),
        ("down", Mods::None) => Some(if app_cursor { "\x1bOB" } else { "\x1b[B" }),
        ("right", Mods::None) => Some(if app_cursor { "\x1bOC" } else { "\x1b[C" }),
        ("left", Mods::None) => Some(if app_cursor { "\x1bOD" } else { "\x1b[D" }),
        ("insert", Mods::None) => Some("\x1b[2~"),
        ("delete", Mods::None) => Some("\x1b[3~"),
        ("pageup", Mods::None) => Some("\x1b[5~"),
        ("pagedown", Mods::None) => Some("\x1b[6~"),
        ("f1", Mods::None) => Some("\x1bOP"),
        ("f2", Mods::None) => Some("\x1bOQ"),
        ("f3", Mods::None) => Some("\x1bOR"),
        ("f4", Mods::None) => Some("\x1bOS"),
        ("f5", Mods::None) => Some("\x1b[15~"),
        ("f6", Mods::None) => Some("\x1b[17~"),
        ("f7", Mods::None) => Some("\x1b[18~"),
        ("f8", Mods::None) => Some("\x1b[19~"),
        ("f9", Mods::None) => Some("\x1b[20~"),
        ("f10", Mods::None) => Some("\x1b[21~"),
        ("f11", Mods::None) => Some("\x1b[23~"),
        ("f12", Mods::None) => Some("\x1b[24~"),
        ("f13", Mods::None) => Some("\x1b[25~"),
        ("f14", Mods::None) => Some("\x1b[26~"),
        ("f15", Mods::None) => Some("\x1b[28~"),
        ("f16", Mods::None) => Some("\x1b[29~"),
        ("f17", Mods::None) => Some("\x1b[31~"),
        ("f18", Mods::None) => Some("\x1b[32~"),
        ("f19", Mods::None) => Some("\x1b[33~"),
        ("f20", Mods::None) => Some("\x1b[34~"),
        _ => None,
    };
    if let Some(s) = fixed {
        return Some(Cow::Borrowed(s));
    }

    // Caret notation. Ctrl+Shift+letter sends the same control code (xterm does too).
    if matches!(mods, Mods::Ctrl | Mods::CtrlShift)
        && let Some(code) = control_code(key)
    {
        return Some(Cow::Owned((code as char).to_string()));
    }

    if mods != Mods::None {
        let code = modifier_code(ks);
        let modified = match key {
            "up" => Some(format!("\x1b[1;{code}A")),
            "down" => Some(format!("\x1b[1;{code}B")),
            "right" => Some(format!("\x1b[1;{code}C")),
            "left" => Some(format!("\x1b[1;{code}D")),
            "home" => Some(format!("\x1b[1;{code}H")),
            "end" => Some(format!("\x1b[1;{code}F")),
            "f1" => Some(format!("\x1b[1;{code}P")),
            "f2" => Some(format!("\x1b[1;{code}Q")),
            "f3" => Some(format!("\x1b[1;{code}R")),
            "f4" => Some(format!("\x1b[1;{code}S")),
            "f5" => Some(format!("\x1b[15;{code}~")),
            "f6" => Some(format!("\x1b[17;{code}~")),
            "f7" => Some(format!("\x1b[18;{code}~")),
            "f8" => Some(format!("\x1b[19;{code}~")),
            "f9" => Some(format!("\x1b[20;{code}~")),
            "f10" => Some(format!("\x1b[21;{code}~")),
            "f11" => Some(format!("\x1b[23;{code}~")),
            "f12" => Some(format!("\x1b[24;{code}~")),
            "insert" => Some(format!("\x1b[2;{code}~")),
            "delete" => Some(format!("\x1b[3;{code}~")),
            "pageup" => Some(format!("\x1b[5;{code}~")),
            "pagedown" => Some(format!("\x1b[6;{code}~")),
            _ => None,
        };
        if let Some(s) = modified {
            return Some(Cow::Owned(s));
        }
    }

    // Alt as meta: ESC prefix. Uses the typed character so Alt+Shift+a gives ESC A and
    // non-ASCII layouts still work.
    if ks.modifiers.alt && !ks.modifiers.platform {
        if ks.modifiers.control
            && let Some(code) = control_code(key)
        {
            return Some(Cow::Owned(format!("\x1b{}", code as char)));
        }
        let text = ks
            .key_char
            .as_deref()
            .filter(|c| c.chars().count() == 1)
            .map(str::to_string)
            .or_else(|| {
                (key.chars().count() == 1).then(|| {
                    if ks.modifiers.shift {
                        key.to_uppercase()
                    } else {
                        key.to_string()
                    }
                })
            });
        if let Some(text) = text {
            return Some(Cow::Owned(format!("\x1b{text}")));
        }
        if key == "space" {
            return Some(Cow::Borrowed("\x1b "));
        }
    }

    None
}

fn control_code(key: &str) -> Option<u8> {
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return match key {
            "space" => Some(0),
            _ => None,
        };
    }
    match c.to_ascii_lowercase() {
        c @ 'a'..='z' => Some(c as u8 - b'a' + 1),
        '@' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// xterm's PC-style modifier parameter: 1 + (shift | alt << 1 | ctrl << 2).
fn modifier_code(ks: &Keystroke) -> u32 {
    let m = &ks.modifiers;
    1 + (m.shift as u32) + ((m.alt as u32) << 1) + ((m.control as u32) << 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn esc(s: &str, mode: TermMode) -> Option<String> {
        to_esc_str(&Keystroke::parse(s).unwrap(), mode).map(|c| c.into_owned())
    }

    #[test]
    fn cursor_keys() {
        assert_eq!(esc("up", TermMode::NONE).as_deref(), Some("\x1b[A"));
        assert_eq!(esc("up", TermMode::APP_CURSOR).as_deref(), Some("\x1bOA"));
        assert_eq!(
            esc("ctrl-left", TermMode::NONE).as_deref(),
            Some("\x1b[1;5D")
        );
        assert_eq!(
            esc("shift-home", TermMode::NONE).as_deref(),
            Some("\x1b[1;2H")
        );
    }

    #[test]
    fn control() {
        assert_eq!(esc("ctrl-c", TermMode::NONE).as_deref(), Some("\x03"));
        assert_eq!(esc("ctrl-shift-a", TermMode::NONE).as_deref(), Some("\x01"));
        assert_eq!(esc("alt-b", TermMode::NONE).as_deref(), Some("\x1bb"));
        assert_eq!(esc("a", TermMode::NONE), None);
    }
}
