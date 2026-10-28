//! Runs extension test cases (`docs/extensions.md`, "Testing an extension").

mod call;
mod format;
mod run;

pub use self::{
    call::run_call,
    format::{CallCase, Case, Input, SessionCase},
    run::CaseRun,
};

/// The outcome of running one extension test case.
#[derive(Debug, Eq, PartialEq)]
pub enum Verdict {
    /// The case passed.
    Pass,
    /// The case ran and failed these expectations, in order.
    Fail(Vec<String>),
    /// The case could not be parsed or validated; the failures are in order.
    Malformed(Vec<String>),
}
