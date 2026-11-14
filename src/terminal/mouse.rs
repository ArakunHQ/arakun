//! Mouse reporting for TUIs (X10/normal, UTF-8 and SGR encodings), ported from
//! alacritty via Zed's `terminal/src/mappings/mouse.rs`.

use alacritty_terminal::term::TermMode;
use gpui::{Modifiers, MouseButton};

/// A viewport cell (0-based, not affected by scrollback).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Copy)]
pub enum Button {
    Left = 0,
    Middle = 1,
    Right = 2,
    LeftMove = 32,
    MiddleMove = 33,
    RightMove = 34,
    NoneMove = 35,
    ScrollUp = 64,
    ScrollDown = 65,
}

impl Button {
    pub fn press(button: MouseButton) -> Option<Self> {
        match button {
            MouseButton::Left => Some(Self::Left),
            MouseButton::Middle => Some(Self::Middle),
            MouseButton::Right => Some(Self::Right),
            MouseButton::Navigate(_) => None,
        }
    }

    pub fn motion(button: Option<MouseButton>) -> Option<Self> {
        match button {
            Some(MouseButton::Left) => Some(Self::LeftMove),
            Some(MouseButton::Middle) => Some(Self::MiddleMove),
            Some(MouseButton::Right) => Some(Self::RightMove),
            Some(MouseButton::Navigate(_)) => None,
            None => Some(Self::NoneMove),
        }
    }
}

pub fn wants_motion(mode: TermMode, button: Option<MouseButton>) -> bool {
    mode.contains(TermMode::MOUSE_MOTION)
        || (mode.contains(TermMode::MOUSE_DRAG) && button.is_some())
}

pub fn report(
    cell: Cell,
    button: Button,
    pressed: bool,
    modifiers: Modifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let mut code = button as u8;
    if modifiers.shift {
        code += 4;
    }
    if modifiers.alt {
        code += 8;
    }
    if modifiers.control {
        code += 16;
    }

    if mode.contains(TermMode::SGR_MOUSE) {
        let c = if pressed { 'M' } else { 'm' };
        return Some(
            format!("\x1b[<{};{};{}{}", code, cell.column + 1, cell.line + 1, c).into_bytes(),
        );
    }

    let utf8 = mode.contains(TermMode::UTF8_MOUSE);
    // Normal encoding has no release per button: releases are reported as button 3.
    let code = if pressed { code } else { 3 + (code & !3) };
    let max = if utf8 { 2015 } else { 223 };
    if cell.line >= max || cell.column >= max {
        return None;
    }
    let mut msg = vec![b'\x1b', b'[', b'M', 32 + code];
    let mut push = |pos: usize| {
        let pos = 32 + 1 + pos;
        if utf8 && pos >= 128 {
            msg.push((0xC0 + pos / 64) as u8);
            msg.push((0x80 + (pos & 63)) as u8);
        } else {
            msg.push(pos as u8);
        }
    };
    push(cell.column);
    push(cell.line);
    Some(msg)
}

/// Alternate scroll (DECSET 1007): wheel → cursor keys in the alt screen.
pub fn alt_scroll(lines: i32, app_cursor: bool) -> Vec<u8> {
    let cmd = if lines > 0 { b'A' } else { b'B' };
    let prefix = if app_cursor { b'O' } else { b'[' };
    let mut out = Vec::with_capacity(lines.unsigned_abs() as usize * 3);
    for _ in 0..lines.unsigned_abs() {
        out.extend_from_slice(&[0x1b, prefix, cmd]);
    }
    out
}
