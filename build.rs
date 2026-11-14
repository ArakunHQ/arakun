fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    // libcef.dll (~200 MB) is only needed once a browser tab opens. Delay-loading it keeps
    // it out of startup, and lets the `arakun <command>` copy in %LOCALAPPDATA%\arakun\bin,
    // which has no CEF next to it, start at all. src/browser/loader.rs loads it by path.
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");
    if msvc && std::env::var_os("CARGO_FEATURE_BROWSER").is_some() {
        println!("cargo::rustc-link-arg=/DELAYLOAD:libcef.dll");
        // gpui embeds arakun.exe's manifest; the helper links no gpui, and Chromium wants
        // the Windows 10 compatibility entry (otherwise it is told it runs on Windows 8).
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("src/bin/arakun-helper.manifest");
        println!("cargo::rerun-if-changed={}", manifest.display());
        println!("cargo::rustc-link-arg-bin=arakun-helper=/MANIFEST:EMBED");
        println!("cargo::rustc-link-arg-bin=arakun-helper=/MANIFESTINPUT:{}", manifest.display());
    }
}
