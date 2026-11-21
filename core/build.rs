fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    set_version();
}

/// `ARAKUN_VERSION`, the version this crate tells agents and servers: the
/// crate's own, or the one the release workflow gives a nightly build. The
/// app's build.rs and server/build.rs do the same.
fn set_version() {
    println!("cargo::rerun-if-env-changed=ARAKUN_VERSION");
    let version = std::env::var("ARAKUN_VERSION")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap());
    println!("cargo::rustc-env=ARAKUN_VERSION={}", version.trim());
}
