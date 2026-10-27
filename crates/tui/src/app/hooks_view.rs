//! The `/hooks` view: one row per hook and watcher the session
//! registered, in its order (`docs/tui.md`, "Swapped views").

use contract::events::HookInfo;

use crate::swapped::{Frame, Ink, List};
use crate::{text, theme::Role};

/// Renders the three ruled columns at `width`: the source, the point or
/// watcher kinds, and the blocking policy. The policy column stays whole;
/// the source and the point-or-kinds truncate to the room that is left.
pub(crate) fn frame(rows: &[HookInfo], list: List, width: u16) -> Frame {
    let body = if rows.is_empty() {
        vec![(vec![("No hooks".to_owned(), None, Ink::Muted)])]
    } else {
        let width = usize::from(width);
        rows.iter().map(|row| row_cells(row, width)).collect()
    };
    Frame {
        title: "Hooks".to_owned(),
        command: "/hooks".to_owned(),
        rows: body,
        list,
        below: Vec::new(),
        field: None,
        footer: "Esc close".to_owned(),
    }
}

/// One row's source, middle and policy together: the declared entry's name
/// else the extension's name; the hook's point or the watcher's kinds joined
/// with `", "`; `blocking`, `non-blocking` or `watcher`, always drawn whole.
fn columns(row: &HookInfo) -> (&str, String, &'static str) {
    match row {
        HookInfo::Hook {
            extension,
            entry,
            point,
            on_failure,
            ..
        } => (
            entry.as_deref().unwrap_or(extension),
            point.clone(),
            if on_failure == "blocking" {
                "blocking"
            } else {
                "non-blocking"
            },
        ),
        HookInfo::Watcher {
            extension,
            entry,
            kinds,
            ..
        } => (
            entry.as_deref().unwrap_or(extension),
            kinds.join(", "),
            "watcher",
        ),
    }
}

fn policy_ink(policy: &str) -> Ink {
    match policy {
        "blocking" => Ink::Role(Role::Error),
        "non-blocking" => Ink::Plain,
        _ => Ink::Muted,
    }
}

/// One row's cells at `width`: the policy stays whole while the source
/// and the middle share what is left.
fn row_cells(row: &HookInfo, width: usize) -> Vec<(String, Option<crate::swapped::Spot>, Ink)> {
    let (source_full, middle_full, policy) = columns(row);
    // The gutter, the two ` · ` separators and the whole policy column.
    let fixed = crate::swapped::GUTTER + policy.len() + 6;
    let room = width.saturating_sub(fixed);
    let source_room = room / 2;
    let middle_room = room.saturating_sub(source_room);
    let source = text::cut(source_full, source_room, "…");
    let middle = text::cut(&middle_full, middle_room, "…");
    let line = format!("  {source} · {middle} · {policy}");
    vec![(line, None, policy_ink(policy))]
}

#[cfg(test)]
#[path = "hooks_view_tests.rs"]
mod tests;
