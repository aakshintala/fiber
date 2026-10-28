//! Runs extension test cases (`docs/extensions.md`, "Testing an extension").

pub mod format;
pub mod run;

pub use format::{CallCase, Case, ScriptCheck, SessionCase};
pub use run::CaseRun;
