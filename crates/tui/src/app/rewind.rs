//! The `/rewind` view (`docs/tui.md`, "Rewind"): the conversation's turns
//! back through the `forked_from` chain, each opening into its step
//! boundaries, with the point's files, calls and running jobs below.

use crate::swapped::{Frame, List};

/// The rewind view's state: the folded turns, the chain pager, the
/// selection, the summary checkbox, and what the view is waiting on.
#[derive(Debug, Default)]
pub(crate) struct Rewind;

/// The view's frame: the title, the command that opened it, and the
/// turn rows with the open selection.
pub(crate) fn frame(list: List) -> Frame {
    Frame {
        title: "Rewind".to_owned(),
        command: "/rewind".to_owned(),
        rows: Vec::new(),
        list,
        below: Vec::new(),
        field: None,
        footer: "↑↓ move · Esc close".to_owned(),
    }
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
