use contract::ErrorCode;

use super::{startup_home_with, startup_with, startup_workspace_with};

#[test]
fn home_failure_keeps_its_code_and_sentence() {
    let failure = startup_home_with(|| Err(config::ConfigError::FiberHome("FIBER_HOME is empty.")))
        .unwrap_err();
    assert_eq!(failure.code, ErrorCode::Usage);
    assert_eq!(failure.message, "FIBER_HOME is empty.");
}

#[test]
fn workspace_failure_names_the_current_directory() {
    let failure = startup_workspace_with(|| Err(std::io::Error::other("gone"))).unwrap_err();
    assert_eq!(failure.code, ErrorCode::IoFailed);
    assert_eq!(failure.message, "the current directory: gone");
}

#[test]
fn home_failure_wins_and_the_workspace_is_not_read() {
    let failure = startup_with(
        || Err(config::ConfigError::FiberHome("FIBER_HOME is empty.")),
        || panic!("a home failure leaves the workspace unread"),
    )
    .unwrap_err();
    assert_eq!(failure.code, ErrorCode::Usage);
    assert_eq!(failure.message, "FIBER_HOME is empty.");
}
