//! Runs extension test cases (`docs/extensions.md`, "Testing an extension").

pub mod call;
pub mod format;
pub mod run;

pub use call::run_call;
pub use format::{CallCase, Case, ScriptCheck, SessionCase};
pub use run::CaseRun;

/// The outcome of running one extension test case.
#[derive(Debug, Eq, PartialEq)]
pub struct Verdict {
    /// The failures to print, in order.
    pub failures: Vec<String>,
    /// Whether the case could not be parsed or validated.
    pub malformed: bool,
}
