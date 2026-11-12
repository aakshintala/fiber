//! Startup failures in the terminal entry map to a failing process status.

use crate::support;

#[test]
fn terminal_startup_failure_reports_reason_and_returns_failure_status() {
    let setup = support::Setup::without_provider();
    let run = support::pty::Run::spawn_unseeded(
        &setup,
        120,
        32,
        &[],
        &[("FIBER_HOME", "relative-fiber-home")],
    );
    let exited = run.wait();
    let stderr = String::from_utf8_lossy(&exited.terminal);
    assert_eq!(exited.status.code(), Some(2));
    assert!(
        !stderr.is_empty(),
        "terminal startup failure must write stderr"
    );
    assert!(
        stderr.contains("fiber: FIBER_HOME must be an absolute path."),
        "stderr must name the startup failure: {stderr}"
    );
}
