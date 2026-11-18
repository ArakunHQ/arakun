//! What arakun says when it can't open its window. Most often no graphics driver works:
//! a VM or remote desktop without a GPU, or a missing or broken Vulkan or Direct3D
//! driver. gpui gives back a bare error (or panics, on Windows), and started from the
//! desktop arakun then simply didn't open; this says what is wrong and what to try.

/// Whether `error` says no graphics device or driver could be used. These are gpui's
/// own messages: wgpu's on Linux, Direct3D's on Windows.
fn is_gpu(error: &str) -> bool {
    const MARKERS: [&str; 9] = [
        "gpu adapter",
        "wgpu",
        "vulkan",
        "create surface",
        "surface reports",
        "directx",
        "direct3d",
        "dxgi",
        "metal",
    ];
    let error = error.to_lowercase();
    MARKERS.iter().any(|m| error.contains(m))
}

#[cfg(target_os = "linux")]
const GPU_HELP: &str = "\
Arakun draws its window on the GPU, with Vulkan or OpenGL, and no driver it can use
was found. This happens in a VM or remote desktop without a GPU, or when the graphics
driver is missing or broken. To fix it:
  - Install the Vulkan driver for your GPU: mesa-vulkan-drivers (Debian, Ubuntu, Fedora),
    vulkan-radeon or vulkan-intel (Arch), or the NVIDIA driver.
    `vulkaninfo --summary` should then list your GPU.
  - Without a GPU, Mesa's software driver works, only slower: it is in
    mesa-vulkan-drivers (Debian, Ubuntu, Fedora) and vulkan-swrast (Arch).";

#[cfg(windows)]
const GPU_HELP: &str = "\
Arakun draws its window with Direct3D 11, and no graphics device could be used.
To fix it:
  - Update the graphics driver, from Windows Update or your GPU maker.
  - In a VM, turn on 3D acceleration for it.
  - If the error names DirectComposition, set GPUI_DISABLE_DIRECT_COMPOSITION=1.";

#[cfg(target_os = "macos")]
const GPU_HELP: &str = "\
Arakun draws its window with Metal, and the GPU could not be used.
A Mac that runs a recent macOS has Metal; in a VM, turn on its graphics acceleration.";

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
const GPU_HELP: &str = "Arakun draws its window on the GPU, and no driver it can use was found.";

/// The whole text for `error`, gpui's error with its causes.
fn message(error: &str) -> String {
    let text = format!("arakun: could not open its window: {error}");
    // An error it can't place gets no advice that may well be wrong.
    if is_gpu(error) {
        format!("{text}\n\n{GPU_HELP}")
    } else {
        text
    }
}

const NOTICE_TITLE: &str = "Arakun could not open its window";

/// The desktop notification's body. The advice comes first: a toast shows only its
/// first lines, and the whole text is in the log or the terminal.
fn notice(error: &str) -> String {
    if is_gpu(error) {
        format!("{GPU_HELP}\n\n{error}")
    } else {
        error.to_string()
    }
}

/// Says why the window didn't open and what to try, then exits with 1. Nothing is
/// saved: no window ever showed, so there is nothing to keep.
pub fn exit(error: &str) -> ! {
    use std::io::IsTerminal as _;
    eprintln!("{}", message(error));
    // Started from a launcher, nobody reads stderr (on Windows it goes to arakun.log),
    // so the desktop shows it too.
    if !std::io::stderr().is_terminal() {
        crate::platform::notify_and_wait(
            NOTICE_TITLE,
            &notice(error),
            std::time::Duration::from_secs(5),
        );
    }
    std::process::exit(1)
}

/// Runs gpui's platform setup. On Windows it panics when no Direct3D device can be
/// made; that comes back as an error here, without the panic text and the crash report
/// a bug would send, because it is the user's driver, not a bug in arakun. Any other
/// panic is left alone: the previous hook (telemetry's crash report, the default
/// message with its location) sees it, and it goes on unwinding as before.
pub fn catch<T>(setup: impl FnOnce() -> T) -> Result<T, String> {
    use std::panic;
    use std::sync::Arc;
    let previous = Arc::new(panic::take_hook());
    let setup_thread = std::thread::current().id();
    let hook = Arc::clone(&previous);
    panic::set_hook(Box::new(move |info| {
        let here = std::thread::current().id() == setup_thread;
        if !hides(here, panic_text(info.payload()).as_deref()) {
            hook(info);
        }
    }));
    let made = panic::catch_unwind(panic::AssertUnwindSafe(setup));
    // Dropping this hook drops its share of `previous`, which can then go back as it was.
    drop(panic::take_hook());
    match Arc::try_unwrap(previous) {
        Ok(previous) => panic::set_hook(previous),
        Err(previous) => panic::set_hook(Box::new(move |info| previous(info))),
    }
    match made {
        Ok(made) => Ok(made),
        Err(payload) => match panic_text(payload.as_ref()) {
            Some(text) if is_gpu(&text) => Err(text),
            _ => panic::resume_unwind(payload),
        },
    }
}

/// Whether `catch`'s hook keeps quiet about a panic: only a GPU error, and only on the
/// thread that runs the setup. Another thread's panic is not caught there, so it must
/// still be reported.
fn hides(on_setup_thread: bool, text: Option<&str>) -> bool {
    on_setup_thread && text.is_some_and(is_gpu)
}

/// The text a panic was raised with, if it was raised with text.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> Option<String> {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_errors_from_gpui_are_recognised() {
        for error in [
            "No GPU adapters found",
            "No GPU adapter found that can configure the display surface",
            "Failed to create surface: Failed to create surface for any enabled backend: {}",
            "Failed to create wgpu device: Out of memory",
            "Surface reports no supported texture formats for adapter Llvmpipe",
            "failed to initialize Windows platform: Creating DirectX devices: Getting DXGI adapter",
            "Creating DirectX renderer: Creating DirectComposition",
        ] {
            assert!(is_gpu(error), "{error}");
        }
        assert!(!is_gpu("Failed to get window handle: oops"));
    }

    #[test]
    fn the_message_names_the_error_and_what_to_do() {
        let text = message("No GPU adapters found");
        assert!(text.starts_with("arakun: could not open its window: No GPU adapters found\n"));
        assert!(text.ends_with(GPU_HELP));
        assert_eq!(
            message("something else"),
            "arakun: could not open its window: something else"
        );
    }

    #[test]
    fn the_notice_puts_the_advice_first() {
        assert_eq!(
            notice("No GPU adapters found"),
            format!("{GPU_HELP}\n\nNo GPU adapters found")
        );
        assert_eq!(notice("something else"), "something else");
    }

    #[test]
    fn only_a_gpu_panic_on_the_setup_thread_is_hidden() {
        assert!(hides(
            true,
            Some("Creating DirectX devices: Getting DXGI adapter")
        ));
        assert!(!hides(true, Some("index out of bounds: the len is 0")));
        assert!(!hides(true, None));
        assert!(!hides(false, Some("Creating DirectX devices")));
    }

    // One test, as the panic hook is the whole process's: two of these at once would
    // swap it under each other.
    #[test]
    fn catch_takes_gpu_panics_and_leaves_the_rest() {
        use std::panic;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Stands in for telemetry's hook: counts this thread's panics and passes other
        // tests' on to the default hook.
        let seen = Arc::new(AtomicUsize::new(0));
        let me = std::thread::current().id();
        let default = panic::take_hook();
        let counter = Arc::clone(&seen);
        panic::set_hook(Box::new(move |info| {
            if std::thread::current().id() == me {
                counter.fetch_add(1, Ordering::SeqCst);
            } else {
                default(info);
            }
        }));

        assert_eq!(catch(|| 7), Ok(7));

        let failed: Result<(), String> = catch(|| panic!("Creating DirectX devices"));
        assert_eq!(failed, Err("Creating DirectX devices".into()));
        assert_eq!(seen.load(Ordering::SeqCst), 0, "a GPU panic stays quiet");

        let bug = panic::catch_unwind(|| catch::<()>(|| panic!("index out of bounds")));
        let payload = bug.expect_err("a bug keeps unwinding");
        assert_eq!(
            panic_text(payload.as_ref()).as_deref(),
            Some("index out of bounds")
        );
        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "the previous hook saw the bug"
        );

        let odd = panic::catch_unwind(|| catch::<()>(|| panic::panic_any(3)));
        assert!(
            odd.expect_err("a panic without text keeps unwinding")
                .is::<i32>()
        );
        assert_eq!(seen.load(Ordering::SeqCst), 2);

        // The previous hook is back: a GPU panic outside `catch` reaches it.
        let _ = panic::catch_unwind(|| panic!("Creating DirectX devices"));
        assert_eq!(seen.load(Ordering::SeqCst), 3, "the hook was put back");

        drop(panic::take_hook());
    }
}
