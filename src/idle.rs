//! How long since the person last touched the keyboard or mouse. Phone notices
//! wait until the person is away from the computer; `None` means this desktop
//! doesn't say, and notices go out as before.

use std::time::Duration;

pub fn idle_for() -> Option<Duration> {
    imp::idle_for()
}

/// The time from `last_ms` to `now_ms`. Input read just after `now` is zero
/// idle time, not an underflow.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn since(now_ms: u64, last_ms: u64) -> Duration {
    Duration::from_millis(now_ms.saturating_sub(last_ms))
}

/// The 64-bit tick of the last input, from the 32-bit tick Windows keeps. That
/// one wraps every 49.7 days, so take the value nearest to `now_ms`: a few
/// milliseconds after it (input since `now` was read) or up to 24.8 days before.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn widen(now_ms: u64, last_low: u32) -> u64 {
    let back = (now_ms as u32).wrapping_sub(last_low) as i32;
    now_ms.saturating_add_signed(-i64::from(back))
}

#[cfg(target_os = "macos")]
mod imp {
    use std::time::Duration;

    // CGEventSourceStateID and CGEventType from <CoreGraphics/CGEventTypes.h>.
    const COMBINED_SESSION_STATE: i32 = 0;
    const ANY_INPUT_EVENT_TYPE: u32 = !0;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(source: i32, event_type: u32) -> f64;
    }

    pub fn idle_for() -> Option<Duration> {
        // SAFETY: takes two plain integers and returns a number; no pointers.
        let secs = unsafe {
            CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, ANY_INPUT_EVENT_TYPE)
        };
        Duration::try_from_secs_f64(secs).ok()
    }
}

#[cfg(windows)]
mod imp {
    use std::time::Duration;
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

    pub fn idle_for() -> Option<Duration> {
        let mut info = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: `info` is a valid LASTINPUTINFO with cbSize set, as the call asks.
        if unsafe { GetLastInputInfo(&mut info) } == 0 {
            return None;
        }
        // SAFETY: no arguments.
        let now = unsafe { GetTickCount64() };
        Some(super::since(now, super::widen(now, info.dwTime)))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::sync::Mutex;
    use std::time::Duration;

    pub fn idle_for() -> Option<Duration> {
        // The session the app runs in decides: XWayland's DISPLAY only sees
        // input to X windows, so Wayland asks the desktop.
        if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()) {
            wayland()
        } else if std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty()) {
            x11()
        } else {
            None
        }
    }

    /// The X connection, kept between calls and dropped after an error so the
    /// next call connects again.
    static X11: Mutex<Option<(x11rb::rust_connection::RustConnection, u32)>> = Mutex::new(None);

    fn x11() -> Option<Duration> {
        use x11rb::connection::Connection as _;
        use x11rb::protocol::screensaver::ConnectionExt as _;
        let mut slot = X11.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            let (conn, screen) = x11rb::connect(None).ok()?;
            let root = conn.setup().roots.get(screen)?.root;
            *slot = Some((conn, root));
        }
        let (conn, root) = slot.as_ref()?;
        match conn.screensaver_query_info(*root).map(|c| c.reply()) {
            Ok(Ok(reply)) => Some(Duration::from_millis(reply.ms_since_user_input.into())),
            _ => {
                *slot = None;
                None
            }
        }
    }

    /// Which D-Bus interface answers on this desktop. Found on the first call
    /// and kept for the life of the process, so a desktop with neither pays
    /// for the lookup once.
    enum Bus {
        Unknown,
        Answers(zbus::blocking::Connection, Source),
        Silent,
    }

    #[derive(Clone, Copy)]
    enum Source {
        Mutter,
        ScreenSaver,
    }

    static BUS: Mutex<Bus> = Mutex::new(Bus::Unknown);

    /// Calls run on the UI thread, so a desktop that doesn't answer must not hold it.
    const TIMEOUT: Duration = Duration::from_millis(200);

    fn wayland() -> Option<Duration> {
        let mut bus = BUS.lock().unwrap_or_else(|e| e.into_inner());
        match &*bus {
            Bus::Answers(conn, source) => ask(conn, *source),
            Bus::Silent => None,
            Bus::Unknown => {
                let found = find();
                let idle = found.as_ref().and_then(|(_, _, idle)| *idle);
                *bus = match found {
                    Some((conn, source, _)) => Bus::Answers(conn, source),
                    None => Bus::Silent,
                };
                idle
            }
        }
    }

    fn find() -> Option<(zbus::blocking::Connection, Source, Option<Duration>)> {
        let conn = zbus::blocking::connection::Builder::session()
            .ok()?
            .method_timeout(TIMEOUT)
            .build()
            .ok()?;
        [Source::Mutter, Source::ScreenSaver]
            .into_iter()
            .find_map(|source| ask(&conn, source).map(|idle| (source, idle)))
            .map(|(source, idle)| (conn, source, Some(idle)))
    }

    fn ask(conn: &zbus::blocking::Connection, source: Source) -> Option<Duration> {
        match source {
            Source::Mutter => {
                let ms: u64 = conn
                    .call_method(
                        Some("org.gnome.Mutter.IdleMonitor"),
                        "/org/gnome/Mutter/IdleMonitor/Core",
                        Some("org.gnome.Mutter.IdleMonitor"),
                        "GetIdletime",
                        &(),
                    )
                    .ok()?
                    .body()
                    .deserialize()
                    .ok()?;
                Some(Duration::from_millis(ms))
            }
            Source::ScreenSaver => {
                let secs: u32 = conn
                    .call_method(
                        Some("org.freedesktop.ScreenSaver"),
                        "/org/freedesktop/ScreenSaver",
                        Some("org.freedesktop.ScreenSaver"),
                        "GetSessionIdleTime",
                        &(),
                    )
                    .ok()?
                    .body()
                    .deserialize()
                    .ok()?;
                Some(Duration::from_secs(secs.into()))
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod imp {
    pub fn idle_for() -> Option<std::time::Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_to_duration_saturates() {
        assert_eq!(since(10_000, 4_000), Duration::from_secs(6));
        assert_eq!(since(5_000, 5_000), Duration::ZERO);
        // Input that lands after `now` was read is not negative idle time.
        assert_eq!(since(5_000, 5_100), Duration::ZERO);
    }

    #[test]
    fn widen_follows_the_32_bit_wrap() {
        // No wrap yet.
        assert_eq!(widen(10_000, 4_000), 4_000);
        // `now` has wrapped past 2^32 and the last input was just before it.
        let now = (1u64 << 32) + 1_000;
        assert_eq!(widen(now, u32::MAX - 999), now - 2_000);
        // Input a little after `now` stays after it, so `since` gives zero.
        assert_eq!(widen(5_000, 5_100), 5_100);
        assert_eq!(since(5_000, widen(5_000, 5_100)), Duration::ZERO);
        // A tick from before boot can't be real: never below zero.
        assert_eq!(widen(100, u32::MAX - 100), 0);
    }
}
