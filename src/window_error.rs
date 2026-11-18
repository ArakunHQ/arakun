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

/// Says why the window didn't open and what to try, then exits with 1. Nothing is
/// saved: no window ever showed, so there is nothing to keep.
pub fn exit(error: &str) -> ! {
    eprintln!("{}", message(error));
    std::process::exit(1)
}

/// Runs gpui's platform setup. On Windows it panics when no Direct3D device can be
/// made; that comes back as an error here, without the panic text and the crash report
/// a bug would send, because it is the user's driver, not a bug in arakun.
pub fn catch<T>(setup: impl FnOnce() -> T) -> Result<T, String> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(setup));
    std::panic::set_hook(hook);
    made.map_err(|payload| {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "the window system could not start".into())
    })
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
    fn a_panic_in_setup_comes_back_as_its_text() {
        assert_eq!(catch(|| 7), Ok(7));
        let failed: Result<(), String> = catch(|| panic!("Creating DirectX devices"));
        assert_eq!(failed, Err("Creating DirectX devices".into()));
        let failed: Result<(), String> = catch(|| std::panic::panic_any(3));
        assert_eq!(failed, Err("the window system could not start".into()));
    }
}
