//! Whether agents may click, type and run scripts on any page, not only local
//! ones (`arakun page`). Off by default: those tabs carry the user's signed-in
//! sessions. Turned on in Settings › Privacy › Agents drive any web page.
//!
//! Kept out of `shell` so Settings can read and set it without depending on the
//! shell that shows it.

pub fn any_page() -> bool {
    crate::settings::read(|s| s.general.agents_drive_any_page)
}

pub fn set_any_page(on: bool) {
    crate::settings::change(|s| s.general.agents_drive_any_page = on);
}
