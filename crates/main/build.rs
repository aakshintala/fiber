//! Records the full commit SHA so `fiber --version` can print it
//! (`docs/releasing.md`, "Versions"), and the target triple so
//! `fiber update` downloads the asset for this binary. With no git
//! history, such as a source tarball or a mutation-testing copy, the
//! lookup fails and `FIBER_COMMIT` stays unset, so `fiber --version`
//! prints `fiber <version>`; a present-but-malformed SHA fails the build.

#![allow(clippy::panic, reason = "a build script fails the build by panicking")]
#![allow(
    clippy::print_stdout,
    reason = "cargo reads build-script instructions from stdout"
)]

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let Some(commit) = git_line(&["rev-parse", "HEAD"]) else {
        // No git history: leave `FIBER_COMMIT` unset so `fiber --version`
        // prints `fiber <version>` (`docs/releasing.md`, "Versions").
        return;
    };
    // HEAD on a branch names the ref and does not change when the commit
    // does, so the ref file is watched too. A packed ref has no loose file;
    // cargo then reruns this script every build, which is cheap. Detached
    // HEAD has no symbolic ref.
    if let Some(head) = git_line(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(reference) = git_line(&["symbolic-ref", "HEAD"])
        && let Some(path) = git_line(&["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={path}");
    }
    if is_commit(&commit) {
        println!("cargo:rustc-env=FIBER_COMMIT={commit}");
    } else {
        panic!("FIBER_COMMIT: `{commit}` is not a commit SHA.");
    }
    // Cargo always sets TARGET for a build script; a different target
    // rebuilds anyway, but the env is declared so the script reruns.
    println!("cargo:rerun-if-env-changed=TARGET");
    let Some(target) = std::env::var("TARGET").ok() else {
        panic!("TARGET: cargo always sets TARGET for a build script.");
    };
    println!("cargo:rustc-env=FIBER_TARGET={target}");
}

fn git_line(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Whether `text` is shaped like a commit: nonempty lowercase hex.
fn is_commit(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
