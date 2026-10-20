//! Records the full commit SHA so `fiber --version` can print it
//! (`docs/releasing.md`, "Versions"). A failed lookup or a
//! non-commit-shaped one fails the build: a release binary always names
//! its commit.

#![allow(clippy::panic, reason = "a build script fails the build by panicking")]
#![allow(
    clippy::print_stdout,
    reason = "cargo reads build-script instructions from stdout"
)]

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let Some(commit) = git_line(&["rev-parse", "HEAD"]) else {
        panic!(
            "FIBER_COMMIT: `git rev-parse HEAD` failed; a release binary always names its commit."
        );
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

/// What `git rev-parse HEAD` prints: lowercase hex, empty or not.
fn is_commit(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
