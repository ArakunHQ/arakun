//! A real terminal: alacritty's emulator on a PTY, painted with GPUI.
//!
//! - [`Terminal`] (entity): the shell process, PTY and grid. Keeps running whether
//!   or not anything shows it.
//! - [`TerminalView`] (entity): renders a `Terminal` filling its parent and handles
//!   keyboard, IME, clipboard and mouse.

// The demo example and the app use different parts of the API.
#![allow(dead_code)]

mod colors;
mod element;
mod keys;
mod model;
mod mouse;
mod view;

pub use model::{Terminal, TerminalEvent};
pub use view::TerminalView;
