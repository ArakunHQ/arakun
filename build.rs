use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let browser = std::env::var_os("CARGO_FEATURE_BROWSER").is_some();
    if browser && std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        copy_cef_runtime();
    }
    // libcef.dll (~200 MB) is only needed once a browser tab opens. Delay-loading it keeps
    // it out of startup, and lets the `arakun <command>` copy in %LOCALAPPDATA%\arakun\bin,
    // which has no CEF next to it, start at all. src/browser/loader.rs loads it by path.
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");
    if msvc && browser {
        println!("cargo::rustc-link-arg=/DELAYLOAD:libcef.dll");
        // gpui embeds arakun.exe's manifest; the helper links no gpui, and Chromium wants
        // the Windows 10 compatibility entry (otherwise it is told it runs on Windows 8).
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("src/bin/arakun-helper.manifest");
        println!("cargo::rerun-if-changed={}", manifest.display());
        println!("cargo::rustc-link-arg-bin=arakun-helper=/MANIFEST:EMBED");
        println!(
            "cargo::rustc-link-arg-bin=arakun-helper=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}

/// Copies the CEF runtime next to arakun.exe, where src/browser/loader.rs looks for it.
/// cef-dll-sys does this too, but it takes the folder three levels above its OUT_DIR,
/// and since Cargo 1.100 build scripts run in `build/<package>/<hash>/out`, one level
/// deeper, so its copy lands in `target/<profile>/build`. The parent of the nearest
/// `build` folder is right with both layouts.
fn copy_cef_runtime() {
    // Set by cef-dll-sys (links = "cef_dll_wrapper"), which is why it is a direct
    // dependency: only direct dependents get it, and their build scripts run after it.
    let Some(cef_dir) = std::env::var_os("DEP_CEF_DLL_WRAPPER_CEF_DIR").map(PathBuf::from) else {
        println!("cargo::warning=cef-dll-sys did not say where CEF is; libcef.dll not copied");
        return;
    };
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let Some(profile) = out_dir
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "build"))
        .and_then(Path::parent)
    else {
        println!("cargo::warning=no build folder above {}", out_dir.display());
        return;
    };
    // Again when CEF changes, or when the copy is deleted.
    println!(
        "cargo::rerun-if-changed={}",
        cef_dir.join("libcef.dll").display()
    );
    println!(
        "cargo::rerun-if-changed={}",
        profile.join("libcef.dll").display()
    );
    let copied = copy_files(&cef_dir, profile)
        .and_then(|()| copy_files(&cef_dir.join("locales"), &profile.join("locales")));
    // A running arakun.exe keeps libcef.dll open; the build itself still works.
    if let Err(e) = copied {
        println!(
            "cargo::warning=copying the CEF runtime to {}: {e}",
            profile.display()
        );
    }
}

/// Copies the files (not the folders) of `from` into `to`, skipping those already there
/// with the same size and modification time.
fn copy_files(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let dest = to.join(entry.file_name());
        let same = std::fs::metadata(&dest).is_ok_and(|old| {
            old.len() == meta.len() && old.modified().ok() == meta.modified().ok()
        });
        if !same {
            std::fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}
