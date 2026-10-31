use std::path::PathBuf;
use std::time::Duration;

use super::{START_BOUND, WarmedJig, warm_up, warm_up_within};
use crate::{MUST_SUCCEED_WITHIN, TempDir, fifo};

#[test]
fn start_bound_covers_the_stall() {
    assert!(
        START_BOUND >= Duration::from_secs(30),
        "the start bound covers the seconds-long first-exec stall: {START_BOUND:?}"
    );
    assert!(
        START_BOUND > MUST_SUCCEED_WITHIN,
        "the start bound exceeds the must-succeed bound: {START_BOUND:?} vs {MUST_SUCCEED_WITHIN:?}"
    );
    assert!(
        2 * START_BOUND <= Duration::from_secs(120),
        "twice the start bound fits nextest's 4 x 30 s timeout (`docs/testing.md`, \"Waits and timeouts\"): {START_BOUND:?}"
    );
}

#[test]
fn a_warm_up_with_the_expected_exit_returns() {
    warm_up(PathBuf::from("/bin/sh").as_path(), &["-c", "exit 3"], 3);
}

#[test]
fn a_wrong_exit_panics_naming_the_jig_and_the_start() {
    let binary = PathBuf::from("/bin/sh");
    let shown = binary.display().to_string();
    let failed = std::panic::catch_unwind(|| warm_up(&binary, &["-c", "exit 3"], 2))
        .expect_err("a warm-up with the wrong exit fails");
    let message = panic_message(failed);
    assert!(
        message.contains(&shown),
        "the failure names the jig: {message}"
    );
    assert!(
        message.contains("start"),
        "the failure names the start wait: {message}"
    );
    assert!(
        message.contains('2') && message.contains('3'),
        "the failure names both codes: {message}"
    );
}

#[test]
fn a_missing_binary_panics_naming_the_jig_and_the_start() {
    let binary = PathBuf::from("/bin/sh-no-such-binary");
    let shown = binary.display().to_string();
    let failed = std::panic::catch_unwind(|| warm_up(&binary, &[], 0))
        .expect_err("a warm-up with a missing binary fails");
    let message = panic_message(failed);
    assert!(
        message.contains(&shown),
        "the failure names the jig: {message}"
    );
    assert!(
        message.contains("start"),
        "the failure names the start wait: {message}"
    );
}

#[test]
fn a_start_that_misses_the_bound_names_the_start_not_the_exchange() {
    let dir = TempDir::new("fiber-jig-start");
    let pipe = dir.path().join("pipe");
    fifo(&pipe);
    let command = format!("read x < {}", pipe.display());
    let binary = PathBuf::from("/bin/sh");
    let shown = binary.display().to_string();
    let failed = std::panic::catch_unwind(|| {
        warm_up_within(&binary, &["-c", &command], 0, Duration::from_millis(100));
    })
    .expect_err("a start past its bound fails");
    let message = panic_message(failed);
    assert!(
        message.contains(&shown),
        "the failure names the jig: {message}"
    );
    assert!(
        message.contains("start"),
        "the failure names the start wait, not the exchange: {message}"
    );
}

#[test]
fn warm_warms_before_it_returns() {
    let binary = PathBuf::from("/bin/sh");
    let shown = binary.display().to_string();
    let failed = std::panic::catch_unwind(|| {
        WarmedJig::warm(binary, &["-c", "exit 3"], 2);
    })
    .expect_err("a warm with the wrong exit fails");
    let message = panic_message(failed);
    assert!(
        message.contains(&shown),
        "the failure names the jig: {message}"
    );
    assert!(
        message.contains("start"),
        "the failure names the start wait: {message}"
    );
}

#[test]
fn path_yields_the_warmed_path() {
    let binary = PathBuf::from("/bin/sh");
    let warmed = WarmedJig::warm(binary.clone(), &["-c", "exit 3"], 3);
    assert_eq!(
        warmed.path(),
        binary.as_path(),
        "the accessor yields the path the warm covered"
    );
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else {
        "a non-string panic".to_owned()
    }
}
