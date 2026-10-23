//! Shared support for the `draw` and `hover` jig tests: both run the
//! built example binary beside the test binary over the checked-in
//! fixture, so the path never changes with a checkout.
//!
//! Both tests include this file with `#[path]`, as `paging_open.rs` does
//! with `support/paging_args.rs`.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use fakes::MUST_SUCCEED_WITHIN;

/// The checked-in events file both jig tests draw, so the path never
/// changes with a checkout.
pub(crate) fn events_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/hover.jsonl")
}

/// The example `cargo test` built beside this test binary
/// (`target/<profile>/examples/<name>`), so the test starts no cargo and
/// waits on no build lock.
pub(crate) fn example_path(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary path");
    path.pop();
    path.pop();
    path.join("examples").join(name)
}

/// Runs the built `binary` with `args` and waits for it. The child is in
/// its own process group under a watchdog, so a stall fails naming
/// `finished` and leaves no process behind.
#[track_caller]
pub(crate) fn run_jig(binary: &str, args: &[&str], finished: &str) -> Output {
    fakes::warm_up(&example_path(binary), &[], 2);
    let child = Command::new(example_path(binary))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("the jig binary spawns");
    let watchdog = fakes::Watchdog::group(child.id());
    let output = fakes::wait_output(child, finished);
    watchdog.stand_down(fakes::Deadline::after(MUST_SUCCEED_WITHIN));
    output
}
