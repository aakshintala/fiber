//! Forwards the release-build marker to the `cli` crate: the release
//! workflow sets `FIBER_RELEASE_BUILD` on its binary build steps, and a
//! plain `cargo build` without it is a source build by construction. The
//! marker is watched unconditionally so a cached build never keeps a stale
//! value when the environment changes.

#![allow(
    clippy::print_stdout,
    reason = "cargo reads build-script instructions from stdout"
)]

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=FIBER_RELEASE_BUILD");
    if std::env::var("FIBER_RELEASE_BUILD").is_ok() {
        println!("cargo:rustc-env=FIBER_RELEASE_BUILD=1");
    }
}
