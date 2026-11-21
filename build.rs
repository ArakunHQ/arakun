use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    set_version();
    embed_icon();
    let browser = std::env::var_os("CARGO_FEATURE_BROWSER").is_some();
    if browser && std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        copy_cef_runtime();
    }
    // libcef.dll (~200 MB) is only needed once a browser tab opens. Delay-loading it keeps
    // it out of startup, and lets the `arakun <command>` copy in %LOCALAPPDATA%\arakun\bin,
    // which has no CEF next to it, start at all. core/src/browser/loader.rs loads it by path.
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

/// `ARAKUN_VERSION`, what `arakun --version` says: the crate's version, or the
/// one the release workflow gives a nightly build (`0.5.1-nightly.202611180300`).
/// server/build.rs does the same.
fn set_version() {
    println!("cargo::rerun-if-env-changed=ARAKUN_VERSION");
    let version = std::env::var("ARAKUN_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap());
    println!("cargo::rustc-env=ARAKUN_VERSION={}", version.trim());
}

/// Puts the app icon in arakun.exe. gpui loads icon 1 from the executable for the
/// window and the taskbar; without it Windows shows a blank program icon. Other
/// targets get nothing (embed_resource says it is not building for Windows).
fn embed_icon() {
    println!("cargo::rerun-if-changed=src/arakun.rc");
    println!("cargo::rerun-if-changed=src/arakun.ico");
    let result = embed_resource::compile_for(
        "src/arakun.rc",
        ["arakun"],
        embed_resource::ParamsIncludeDirs(["src"]),
    );
    // A build for Windows from Linux or macOS without windres or llvm-rc still works, but
    // the exe and its window get the blank icon, so say so instead of skipping it quietly.
    // (On a Windows host a missing rc.exe is Failed, and the unwrap below stops the build.)
    if matches!(result, embed_resource::CompilationResult::NotAttempted(_)) {
        let why = result.to_string().replace('\n', " ");
        println!("cargo::warning=arakun.exe gets no icon ({why})");
    }
    result.manifest_optional().unwrap();
}

/// Copies the CEF runtime next to arakun.exe, where core/src/browser/loader.rs looks for it.
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
    // Again when CEF changes. copy_files watches each copy, for when one is deleted.
    let libcef = cef_dir.join("libcef.dll");
    println!("cargo::rerun-if-changed={}", libcef.display());
    // Written only when everything is copied. Cargo runs this script again while it is
    // missing, so a failed copy is retried on the next build.
    let stamp = out_dir.join("cef-runtime-copied");
    println!("cargo::rerun-if-changed={}", stamp.display());
    let _ = std::fs::remove_file(&stamp);
    let copied = copy_files(&cef_dir, profile)
        .and_then(|()| copy_files(&cef_dir.join("locales"), &profile.join("locales")))
        .and_then(|()| write_stamp(&stamp, &libcef));
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
///
/// Cargo runs the script again when a watched file is newer than the start of its last
/// run. So each copy and the stamp keep the (older) time of their source, not the time
/// they were written; otherwise every build would run this script again.
fn copy_files(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let dest = to.join(entry.file_name());
        println!("cargo::rerun-if-changed={}", dest.display());
        let modified = meta.modified()?;
        let same = std::fs::metadata(&dest)
            .is_ok_and(|old| old.len() == meta.len() && old.modified().ok() == Some(modified));
        if !same {
            std::fs::copy(entry.path(), &dest)?;
            // Windows keeps the time when copying; other hosts (cross builds) don't.
            std::fs::File::options()
                .write(true)
                .open(&dest)?
                .set_modified(modified)?;
        }
    }
    Ok(())
}

/// See copy_files for why the stamp takes the time of `like`.
fn write_stamp(stamp: &Path, like: &Path) -> std::io::Result<()> {
    let modified = std::fs::metadata(like)?.modified()?;
    std::fs::File::create(stamp)?.set_modified(modified)
}
