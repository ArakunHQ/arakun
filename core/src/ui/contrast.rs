//! Follows the system's high contrast setting: macOS's Increase contrast,
//! Windows high contrast and GNOME's high contrast theme. It is read at
//! start and again when a window comes to the front, since the setting is
//! usually changed in another app. What it does to the colors is
//! `theme::high_contrast`'s. On Windows the theme's own colors are read
//! too, and drawn in place of the palette (`theme::system_colors`).

use std::sync::atomic::{AtomicU64, Ordering};

use gpui::App;

use crate::theme;

/// Counts the reads started, so a slow one that ends after a newer one
/// can't put back what the system said before.
static READS: AtomicU64 = AtomicU64::new(0);

/// Reads the system's setting off the main thread, and redraws every window
/// if it changed.
pub fn follow_system(cx: &mut App) {
    let read = READS.fetch_add(1, Ordering::Relaxed) + 1;
    cx.spawn(async move |cx| {
        let (on, colors) = cx
            .background_executor()
            .spawn(async {
                let on = system_high_contrast();
                (on, on.then(system_colors).flatten())
            })
            .await;
        if read != READS.load(Ordering::Relaxed)
            || (on, colors) == (theme::system_high_contrast(), theme::system_colors_read())
        {
            return;
        }
        cx.update(|cx| {
            theme::set_system_high_contrast(on);
            theme::set_system_colors(colors);
            apply(cx);
        });
    })
    .detach();
}

/// Redraws every window in the contrast now in force: glass goes off and on
/// with it, and the window background has to follow. A window whose update
/// is running (the caller's own) is skipped; set its background there.
pub fn apply(cx: &mut App) {
    for handle in cx.windows() {
        let _ = handle.update(cx, |_, window, _| {
            window.set_background_appearance(theme::window_background());
        });
    }
    cx.refresh_windows();
}

fn system_high_contrast() -> bool {
    #[cfg(target_os = "macos")]
    {
        super::motion::read(
            "defaults",
            &["read", "com.apple.universalaccess", "increaseContrast"],
        )
        .is_some_and(|out| out == "1")
    }
    #[cfg(target_os = "linux")]
    {
        // GNOME 42 and later have a switch; older ones and other GTK desktops
        // pick a theme named for it.
        super::motion::read(
            "gsettings",
            &["get", "org.gnome.desktop.a11y.interface", "high-contrast"],
        )
        .is_some_and(|out| out == "true")
            || super::motion::read(
                "gsettings",
                &["get", "org.gnome.desktop.interface", "gtk-theme"],
            )
            .is_some_and(|out| gtk_theme_is_high_contrast(&out))
    }
    #[cfg(windows)]
    {
        windows_high_contrast()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        false
    }
}

/// `'HighContrast'`, `'HighContrastInverse'`: what gsettings prints for
/// GNOME's own high contrast themes.
#[cfg(any(target_os = "linux", test))]
fn gtk_theme_is_high_contrast(out: &str) -> bool {
    out.trim_matches('\'')
        .to_ascii_lowercase()
        .contains("highcontrast")
}

#[cfg(windows)]
fn windows_high_contrast() -> bool {
    use windows_sys::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETHIGHCONTRAST, SystemParametersInfoW};
    let mut hc = HIGHCONTRASTW {
        cbSize: std::mem::size_of::<HIGHCONTRASTW>() as u32,
        dwFlags: 0,
        lpszDefaultScheme: std::ptr::null_mut(),
    };
    // SAFETY: `hc` is a HIGHCONTRASTW with its size set, as the call asks.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            hc.cbSize,
            (&mut hc as *mut HIGHCONTRASTW).cast(),
            0,
        )
    };
    ok != 0 && hc.dwFlags & HCF_HIGHCONTRASTON != 0
}

/// The colors of the high contrast theme in force, on Windows only: macOS
/// and GNOME raise contrast without handing apps a set of colors.
fn system_colors() -> Option<theme::SystemColors> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Graphics::Gdi::{
            COLOR_HOTLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT, GetSysColor,
        };
        // SAFETY: GetSysColor takes a color index and has no other inputs.
        let get = |ix| colorref_to_rgb(unsafe { GetSysColor(ix) });
        Some(theme::SystemColors::new(
            get(COLOR_WINDOW),
            get(COLOR_WINDOWTEXT),
            get(COLOR_HOTLIGHT),
        ))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// A Windows COLORREF (`0x00BBGGRR`) as `0xRRGGBB`.
#[cfg(any(windows, test))]
fn colorref_to_rgb(c: u32) -> u32 {
    let (r, g, b) = (c & 0xff, (c >> 8) & 0xff, (c >> 16) & 0xff);
    r << 16 | g << 8 | b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_high_contrast_themes_are_recognized() {
        assert!(gtk_theme_is_high_contrast("'HighContrast'"));
        assert!(gtk_theme_is_high_contrast("'HighContrastInverse'"));
        assert!(!gtk_theme_is_high_contrast("'Adwaita-dark'"));
    }

    #[test]
    fn colorrefs_turn_into_rgb() {
        // Desert's hyperlink, #1C5E75, as GetSysColor returns it.
        assert_eq!(colorref_to_rgb(0x00755e1c), 0x1c5e75);
        assert_eq!(colorref_to_rgb(0x000000ff), 0xff0000);
    }
}
