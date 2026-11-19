//! `ARAKUN_VERSION`, as the app's build.rs sets it: the server shares the
//! app's source files, which print it.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=ARAKUN_VERSION");
    let version = std::env::var("ARAKUN_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap());
    println!("cargo::rustc-env=ARAKUN_VERSION={}", version.trim());
}
