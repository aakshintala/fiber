//! The feedback seam (`docs/architecture.md`, "The call rules"): the
//! report builder lives in `cli`, which `tui` may not call, so `contract`
//! defines the writer type and `main` injects it. Vocabulary only: no
//! behaviour beyond the types both sides name.

use std::path::PathBuf;
use std::sync::Arc;

use crate::SessionId;

/// Writes a feedback report for `description` and the session on screen,
/// when there is one, and returns its path with the issue link: the
/// injected writer `main` supplies, so the TUI names this seam and never
/// `cli`.
pub type WriteFeedback =
    Arc<dyn Fn(&str, Option<&SessionId>) -> Result<FeedbackDone, String> + Send + Sync>;

/// What a feedback write returns: the report's path and its issue link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackDone {
    /// The report file written under the current directory.
    pub path: PathBuf,
    /// The new-issue link carrying the report, or its description.
    pub link: String,
}
