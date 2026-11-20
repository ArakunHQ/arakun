//! Follows the system's high contrast setting: macOS's Increase contrast,
//! Windows high contrast and GNOME's high contrast theme. It is read at
//! start and again when a window comes to the front, since the setting is
//! usually changed in another app. What it does to the colors is
//! `theme::high_contrast`'s.

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
        let on = cx
            .background_executor()
            .spawn(async { system_high_contrast() })
            .await;
        if read != READS.load(Ordering::Relaxed) || on == theme::system_high_contrast() {
            return;
        }
        cx.update(|cx| {
            theme::set_system_high_contrast(on);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gnome_high_contrast_themes_are_recognized() {
        assert!(gtk_theme_is_high_contrast("'HighContrast'"));
        assert!(gtk_theme_is_high_contrast("'HighContrastInverse'"));
        assert!(!gtk_theme_is_high_contrast("'Adwaita-dark'"));
    }
}
