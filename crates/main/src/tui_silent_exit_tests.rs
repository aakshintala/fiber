//! Tests for how the main terminal entry handles TUI exit results.

use std::fs::File;
use std::sync::Arc;

#[test]
fn tui_startup_error_keeps_its_cause_for_the_terminal_exit() {
    let tty = File::open("/dev/null").unwrap_or_else(|error| panic!("open /dev/null: {error}"));
    let clock = fakes::clock::FakeClock::new();
    let result = tui::run(
        tty,
        tui::Launch::default(),
        Box::new(|| Err(std::io::Error::other("unused"))),
        Box::new(|_| {}),
        clock.clone(),
        Arc::new(log::diag::Diag::new(
            &std::env::temp_dir(),
            log::diag::Process::Tui,
            log::diag::Level::Info,
            clock,
        )),
        None,
    );
    let error = result.unwrap_err();
    assert_eq!(error.code(), contract::ErrorCode::IoFailed);
    assert!(
        error.to_string().contains("terminal") && !error.to_string().contains("could not continue"),
        "the returned error keeps the setup cause: {error}"
    );
}
