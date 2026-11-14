//! GPUI keystrokes → CEF key events.

use cef::{KeyEvent, KeyEventType};
use gpui::Modifiers;

// cef_event_flags_t values from cef_types.h.
pub const SHIFT: u32 = 1 << 1;
pub const CONTROL: u32 = 1 << 2;
pub const ALT: u32 = 1 << 3;
pub const LEFT_BUTTON: u32 = 1 << 4;
pub const MIDDLE_BUTTON: u32 = 1 << 5;
pub const RIGHT_BUTTON: u32 = 1 << 6;
pub const COMMAND: u32 = 1 << 7;

pub fn flags(m: &Modifiers) -> u32 {
    let mut f = 0;
    if m.shift {
        f |= SHIFT;
    }
    if m.control {
        f |= CONTROL;
    }
    if m.alt {
        f |= ALT;
    }
    if m.platform {
        f |= COMMAND;
    }
    f
}

pub fn event(type_: KeyEventType, key: &str, modifiers: u32, ch: u16) -> KeyEvent {
    KeyEvent {
        size: std::mem::size_of::<cef::sys::_cef_key_event_t>(),
        type_,
        modifiers,
        windows_key_code: windows_key_code(key),
        native_key_code: native_key_code(key),
        is_system_key: 0,
        character: ch,
        unmodified_character: ch,
        focus_on_editable_field: 0,
    }
}

/// A CHAR event carrying one UTF-16 unit of committed text.
pub fn char_event(unit: u16, modifiers: u32) -> KeyEvent {
    KeyEvent {
        size: std::mem::size_of::<cef::sys::_cef_key_event_t>(),
        type_: KeyEventType::CHAR,
        modifiers,
        windows_key_code: unit as i32,
        native_key_code: 0,
        is_system_key: 0,
        character: unit,
        unmodified_character: unit,
        focus_on_editable_field: 0,
    }
}

/// Windows virtual-key code for a GPUI key name: what Blink's keydown handling, `e.keyCode`
/// and the editing shortcuts (Ctrl+A/C/V/Z) key off.
pub fn windows_key_code(key: &str) -> i32 {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        let u = c.to_ascii_uppercase();
        if u.is_ascii_alphanumeric() {
            return u as i32;
        }
        return match c {
            ' ' => 0x20,
            ';' | 'ñ' => 0xBA,
            '=' | '¡' => 0xBB,
            ',' => 0xBC,
            '-' => 0xBD,
            '.' => 0xBE,
            '/' => 0xBF,
            '`' | 'º' => 0xC0,
            '[' => 0xDB,
            '\\' | 'ç' => 0xDC,
            ']' | '+' => 0xDD,
            '\'' => 0xDE,
            '<' => 0xE2,
            _ => 0,
        };
    }
    match key {
        "enter" => 0x0D,
        "backspace" => 0x08,
        "tab" => 0x09,
        "escape" => 0x1B,
        "space" => 0x20,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "end" => 0x23,
        "home" => 0x24,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "insert" => 0x2D,
        "delete" => 0x2E,
        "shift" => 0x10,
        "control" => 0x11,
        "alt" => 0x12,
        "capslock" => 0x14,
        "platform" | "super" => 0x5B,
        "menu" => 0x5D,
        k if k.starts_with('f') && k.len() <= 3 => k[1..].parse::<i32>().map_or(0, |n| 0x6F + n),
        _ => 0,
    }
}

/// What CEF reads as the platform's own key code, from which Chromium derives
/// `KeyboardEvent.code`: an XKB keycode on Linux, a `WM_KEYDOWN` lParam on Windows, a
/// virtual key code on macOS.
pub fn native_key_code(key: &str) -> i32 {
    if cfg!(windows) {
        windows_lparam(key)
    } else if cfg!(target_os = "macos") {
        mac_key_code(key)
    } else {
        xkb_keycode(key)
    }
}

/// The macOS virtual key code (`kVK_*` from Carbon's Events.h) for a GPUI key name,
/// found through its evdev scancode so both share the US/ES QWERTY layout assumption.
/// Unknown keys get 0xFFFF, since 0 is `kVK_ANSI_A`.
pub fn mac_key_code(key: &str) -> i32 {
    const BY_EVDEV: &[(i32, i32)] = &[
        (1, 0x35),   // escape
        (2, 0x12),   // 1
        (3, 0x13),   // 2
        (4, 0x14),   // 3
        (5, 0x15),   // 4
        (6, 0x17),   // 5
        (7, 0x16),   // 6
        (8, 0x1A),   // 7
        (9, 0x1C),   // 8
        (10, 0x19),  // 9
        (11, 0x1D),  // 0
        (12, 0x1B),  // -
        (13, 0x18),  // =
        (14, 0x33),  // backspace
        (15, 0x30),  // tab
        (16, 0x0C),  // q
        (17, 0x0D),  // w
        (18, 0x0E),  // e
        (19, 0x0F),  // r
        (20, 0x11),  // t
        (21, 0x10),  // y
        (22, 0x20),  // u
        (23, 0x22),  // i
        (24, 0x1F),  // o
        (25, 0x23),  // p
        (26, 0x21),  // [
        (27, 0x1E),  // ]
        (28, 0x24),  // enter
        (29, 0x3B),  // control
        (30, 0x00),  // a
        (31, 0x01),  // s
        (32, 0x02),  // d
        (33, 0x03),  // f
        (34, 0x05),  // g
        (35, 0x04),  // h
        (36, 0x26),  // j
        (37, 0x28),  // k
        (38, 0x25),  // l
        (39, 0x29),  // ;
        (40, 0x27),  // '
        (41, 0x32),  // `
        (42, 0x38),  // shift
        (43, 0x2A),  // backslash
        (44, 0x06),  // z
        (45, 0x07),  // x
        (46, 0x08),  // c
        (47, 0x09),  // v
        (48, 0x0B),  // b
        (49, 0x2D),  // n
        (50, 0x2E),  // m
        (51, 0x2B),  // ,
        (52, 0x2F),  // .
        (53, 0x2C),  // /
        (56, 0x3A),  // alt (option)
        (57, 0x31),  // space
        (58, 0x39),  // capslock
        (59, 0x7A),  // f1
        (60, 0x78),  // f2
        (61, 0x63),  // f3
        (62, 0x76),  // f4
        (63, 0x60),  // f5
        (64, 0x61),  // f6
        (65, 0x62),  // f7
        (66, 0x64),  // f8
        (67, 0x65),  // f9
        (68, 0x6D),  // f10
        (86, 0x0A),  // < (ISO section key)
        (87, 0x67),  // f11
        (88, 0x6F),  // f12
        (102, 0x73), // home
        (103, 0x7E), // up
        (104, 0x74), // pageup
        (105, 0x7B), // left
        (106, 0x7C), // right
        (107, 0x77), // end
        (108, 0x7D), // down
        (109, 0x79), // pagedown
        (110, 0x72), // insert (help)
        (111, 0x75), // delete (forward delete)
        (125, 0x37), // platform (command)
    ];
    let evdev = xkb_keycode(key) - 8;
    BY_EVDEV.iter().find(|(e, _)| *e == evdev).map_or(0xFFFF, |(_, code)| *code)
}

/// The `WM_KEYDOWN` lParam Windows would send for a GPUI key name: repeat count 1, the
/// set-1 scancode in bits 16-23 and the extended-key flag in bit 24. Set-1 scancodes are
/// the evdev codes for the main block; the navigation keys are `E0`-prefixed.
pub fn windows_lparam(key: &str) -> i32 {
    let evdev = xkb_keycode(key) - 8;
    if evdev <= 0 {
        return 0;
    }
    let (scan, extended) = match evdev {
        102 => (0x47, true), // home
        103 => (0x48, true), // up
        104 => (0x49, true), // pageup
        105 => (0x4B, true), // left
        106 => (0x4D, true), // right
        107 => (0x4F, true), // end
        108 => (0x50, true), // down
        109 => (0x51, true), // pagedown
        110 => (0x52, true), // insert
        111 => (0x53, true), // delete
        125 => (0x5B, true), // left Windows key
        127 => (0x5D, true), // menu
        code if code <= 0x58 => (code, false),
        _ => return 0,
    };
    1 | (scan << 16) | (i32::from(extended) << 24)
}

/// XKB keycode (evdev scancode + 8) for a GPUI key name, assuming the physical layout of a
/// US/ES QWERTY keyboard. Chromium turns it into `KeyboardEvent.code`.
pub fn xkb_keycode(key: &str) -> i32 {
    const ROW_Q: &str = "qwertyuiop";
    const ROW_A: &str = "asdfghjkl";
    const ROW_Z: &str = "zxcvbnm";
    let mut chars = key.chars();
    let evdev = if let (Some(c), None) = (chars.next(), chars.next()) {
        let c = c.to_ascii_lowercase();
        if let Some(i) = ROW_Q.find(c) {
            16 + i as i32
        } else if let Some(i) = ROW_A.find(c) {
            30 + i as i32
        } else if let Some(i) = ROW_Z.find(c) {
            44 + i as i32
        } else {
            match c {
                '1'..='9' => 2 + (c as i32 - '1' as i32),
                '0' => 11,
                '-' | '\'' => 12,
                '=' | '¡' => 13,
                '[' | '`' => 26,
                ']' | '+' => 27,
                ';' | 'ñ' => 39,
                '´' => 40,
                'º' => 41,
                '\\' | 'ç' => 43,
                ',' => 51,
                '.' => 52,
                '/' => 53,
                ' ' => 57,
                '<' => 86,
                _ => return 0,
            }
        }
    } else {
        match key {
            "escape" => 1,
            "backspace" => 14,
            "tab" => 15,
            "enter" => 28,
            "control" => 29,
            "shift" => 42,
            "alt" => 56,
            "space" => 57,
            "capslock" => 58,
            "f1" => 59,
            "f2" => 60,
            "f3" => 61,
            "f4" => 62,
            "f5" => 63,
            "f6" => 64,
            "f7" => 65,
            "f8" => 66,
            "f9" => 67,
            "f10" => 68,
            "f11" => 87,
            "f12" => 88,
            "home" => 102,
            "up" => 103,
            "pageup" => 104,
            "left" => 105,
            "right" => 106,
            "end" => 107,
            "down" => 108,
            "pagedown" => 109,
            "insert" => 110,
            "delete" => 111,
            "platform" | "super" => 125,
            "menu" => 127,
            _ => return 0,
        }
    };
    evdev + 8
}

/// The UTF-16 unit Chromium expects in `character` for keys whose RAWKEYDOWN also carries
/// one (it becomes `KeyboardEvent.key`).
pub fn key_character(key: &str, key_char: Option<&str>) -> u16 {
    match key {
        "enter" => 13,
        "tab" => 9,
        "backspace" => 8,
        "escape" => 27,
        _ => key_char
            .and_then(|s| {
                let mut units = s.encode_utf16();
                let first = units.next()?;
                units.next().is_none().then_some(first)
            })
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_lparam_carries_set1_scancodes() {
        assert_eq!(windows_lparam("a"), 1 | (0x1E << 16));
        assert_eq!(windows_lparam("enter"), 1 | (0x1C << 16));
        assert_eq!(windows_lparam("f12"), 1 | (0x58 << 16));
        assert_eq!(windows_lparam("left"), 1 | (0x4B << 16) | (1 << 24));
        assert_eq!(windows_lparam("delete"), 1 | (0x53 << 16) | (1 << 24));
        assert_eq!(windows_lparam("unknown"), 0);
    }

    #[test]
    fn mac_key_codes_follow_carbon() {
        assert_eq!(mac_key_code("a"), 0x00);
        assert_eq!(mac_key_code("enter"), 0x24);
        assert_eq!(mac_key_code("backspace"), 0x33);
        assert_eq!(mac_key_code("left"), 0x7B);
        assert_eq!(mac_key_code("f12"), 0x6F);
        assert_eq!(mac_key_code("unknown"), 0xFFFF);
    }
}
