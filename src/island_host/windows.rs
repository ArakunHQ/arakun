//! The island host on Windows. There is no layer shell to anchor the island to
//! a screen edge, so its window is a topmost popup (the GPUI fork makes it
//! frameless, never focused and click-through outside its shape) placed by
//! hand at the top center of the primary display's work area.

use gpui::{App, WindowBounds, WindowOptions};

/// Puts the island's window at the top center of its display.
pub fn place(cx: &App, options: &mut WindowOptions) {
    if let Some(at) = crate::island::top_center(cx, options.display_id) {
        options.window_bounds = Some(WindowBounds::Windowed(at));
    }
}
