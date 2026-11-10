//! Warming a jig's built binary past its first-exec stall, for tests.
//!
//! The first exec of a freshly linked binary can stall for seconds on macOS
//! (`docs/testing.md`, "Waits and timeouts" states the stall), so every jig test runs its
//! binary once on fast-failing arguments under [`START_BOUND`] before the
//! exchange keeps its own deadline. No retry and no wider exchange deadline:
//! the warm-up either absorbs the stall or fails naming the start.

use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// How long a jig binary's first start may take: at least 30 s, above
/// `MUST_SUCCEED_WITHIN`, and twice it fits nextest's 4 x 30 s timeout
/// (`docs/testing.md`, "Waits and timeouts"). Sized from the worst observed
/// stand-in stall of 28.36 s with margin, and capped so a missed warm-up
/// with its reap still reports before the harness kills the test.
pub const START_BOUND: Duration = Duration::from_secs(50);

/// A jig binary's path past its first-exec stall: the only constructor
/// warms through [`warm_up`] before returning, so no value exists that
/// skipped the warm and every exec site builds from [`WarmedJig::path`].
pub struct WarmedJig {
    path: PathBuf,
}

impl WarmedJig {
    /// Warms `path` by running it with `args` and asserting its exit is
    /// `exit`, then holds the warmed path. A jig example binary starts
    /// through this; [`warm_up`] is for an already-built `CARGO_BIN_EXE_*`
    /// product binary and the checked-in MCP fixture script. The freshness
    /// refusal runs before the spawn, so a stale binary never executes.
    #[track_caller]
    pub fn warm(path: PathBuf, args: &[&str], exit: i32) -> Self {
        assert_jig_fresh(&path);
        warm_up(&path, args, exit);
        Self { path }
    }

    /// The warmed path to exec.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Refuses a jig example binary older than any file its sibling dep-info
/// file lists. Cargo writes `<binary>.d` beside every built example naming
/// each input the link depended on, so a source touched after a narrowed
/// build that skipped the examples leaves a binary this names instead of
/// running. A missing dep-info file skips the check, as foreign stand-ins
/// such as the system shell have none; every other doubt fails closed,
/// since a silent stale run misleads and a false refusal costs only a
/// rebuild and a re-run.
///
/// # Panics
///
/// When the binary is stale. Names the binary, the evidence, and the
/// rebuild command.
#[allow(
    clippy::panic,
    reason = "a stale jig binary fails the test naming the binary and its rebuild"
)]
#[track_caller]
fn assert_jig_fresh(binary: &Path) {
    if let Some(stale) = stale_evidence(binary) {
        panic!(
            "stale jig binary {}: {} is not older than the binary, missing, or unreadable; rebuild the examples with `cargo build --examples`",
            binary.display(),
            stale.display()
        );
    }
}

/// The stale input for `binary`, or `None` when the binary is fresh or
/// unchecked: no sibling dep-info file means unchecked, never stale. Any
/// listed file missing or dated at or after the binary means stale, as does
/// a dep-info file that cannot be read or parsed.
fn stale_evidence(binary: &Path) -> Option<PathBuf> {
    let dep_info = sibling_dep_info(binary);
    let text = match std::fs::read_to_string(&dep_info) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(dep_info),
    };
    let deps = match dep_info_deps(&text) {
        Some(deps) => deps,
        None => return Some(dep_info),
    };
    let binary_mtime = std::fs::metadata(binary)
        .and_then(|meta| meta.modified())
        .ok()?;
    for dep in deps {
        let dep_path = PathBuf::from(dep);
        let stale = match std::fs::metadata(&dep_path).and_then(|meta| meta.modified()) {
            Ok(dep_mtime) => dep_mtime >= binary_mtime,
            Err(_) => true,
        };
        if stale {
            return Some(dep_path);
        }
    }
    None
}

/// The dep-info file beside `binary`: `<binary>.d`.
fn sibling_dep_info(binary: &Path) -> PathBuf {
    let mut name = binary
        .file_name()
        .map(|name| name.to_owned())
        .unwrap_or_default();
    name.push(".d");
    binary.with_file_name(name)
}

/// The dependencies in a dep-info file's first make rule (`target: deps`,
/// backslash escapes), keeping every listed file: freshness follows cargo's
/// own dirty rule, which is any input.
fn dep_info_deps(text: &str) -> Option<Vec<String>> {
    let rule = text
        .lines()
        .take_while(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let (target, deps) = rule.split_once(':')?;
    if target.trim().is_empty() {
        return None;
    }
    let deps = make_words(deps);
    if deps.is_empty() {
        return None;
    }
    Some(deps)
}

/// Split the escaped dependency words in cargo's first makefile rule.
fn make_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            word.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(ch);
        }
    }
    if escaped {
        word.push('\\');
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Runs `binary args` once under [`START_BOUND`] on every call and asserts
/// its exit is `exit`: no state, so a failed warm-up affects no later call.
/// The child leads its own process group and a watchdog guards it, as the
/// sibling jig wrappers do (`docs/testing/running.md`). For an already-built
/// `CARGO_BIN_EXE_*` product binary and the checked-in MCP fixture script
/// (`mcp_fixture`); a jig example binary starts through [`WarmedJig::warm`].
///
/// # Panics
///
/// When the binary cannot spawn, misses the bound, or exits otherwise. Each
/// names the start of the jig's path, never the exchange.
#[allow(
    clippy::panic,
    reason = "a jig binary that cannot start fails the test naming the start"
)]
#[track_caller]
pub fn warm_up(binary: &Path, args: &[&str], exit: i32) {
    warm_up_within(binary, args, exit, START_BOUND);
}

/// The same run with the bound injected: the bound only sets how soon a
/// stalled start fails by design.
#[allow(
    clippy::panic,
    reason = "a jig binary that cannot start fails the test naming the start"
)]
#[track_caller]
pub(crate) fn warm_up_within(binary: &Path, args: &[&str], exit: i32, bound: Duration) {
    let what = format!("the start of {}", binary.display());
    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => panic!("{what} could not spawn: {err}"),
    };
    let watchdog = crate::Watchdog::group(child.id());
    let output = crate::finished(&what, child, &[], crate::Deadline::after(bound));
    watchdog.stand_down(crate::Deadline::after(crate::MUST_SUCCEED_WITHIN));
    assert_eq!(
        output.status.code(),
        Some(exit),
        "{what} exited {} but the warm-up needs {exit}",
        output.status
    );
}

#[cfg(test)]
#[path = "jig_start_tests.rs"]
mod tests;
