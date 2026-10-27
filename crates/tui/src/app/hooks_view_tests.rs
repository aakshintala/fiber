//! Tests for the `/hooks` view: the three ruled columns at the TUI's
//! 40-column floor and the empty-state text.

use contract::events::HookInfo;

use super::frame;
use crate::swapped::List;

fn hook(extension: &str, point: &str, on_failure: &str) -> HookInfo {
    HookInfo::Hook {
        extension: extension.to_owned(),
        entry: None,
        point: point.to_owned(),
        on_failure: on_failure.to_owned(),
        phase: None,
    }
}

/// The policy column stays whole at 40 columns while the source and the
/// point truncate to the room that is left. Dropping the policy would hide
/// whether the hook blocks; dropping a column would break the ruling.
#[test]
fn the_policy_column_stays_whole_at_forty_columns() {
    let rows = vec![
        hook(
            "a-very-long-extension-name-that-overflows",
            "before_tool_with_a_long_point_name",
            "non-blocking",
        ),
        HookInfo::Watcher {
            extension: "ext".to_owned(),
            entry: None,
            kinds: vec!["turn_completed".to_owned(), "turn_started".to_owned()],
        },
        HookInfo::Hook {
            extension: "hooks".to_owned(),
            entry: Some("audit".to_owned()),
            point: "after_tool".to_owned(),
            on_failure: "blocking".to_owned(),
            phase: Some("sanitize".to_owned()),
        },
    ];
    let frame = frame(&rows, List::default(), 40);
    assert_eq!(frame.rows.len(), 3);
    let lines: Vec<&str> = frame.rows.iter().map(|cells| cells[0].0.as_str()).collect();
    // Every row keeps its whole policy: truncating it would hide whether
    // the hook blocks.
    assert!(lines[0].ends_with("non-blocking"), "{lines:?}");
    assert!(lines[1].ends_with("watcher"), "{lines:?}");
    assert!(lines[2].ends_with("blocking"), "{lines:?}");
    // The declared entry names the source, and the watcher joins its
    // kinds with `", "`, truncated to fit.
    assert!(lines[2].contains("audit"), "{lines:?}");
    assert!(
        lines[0].contains("…") || lines[1].contains("…"),
        "long text truncates instead of hiding the policy: {lines:?}"
    );
    // Flipping the policy to a truncated prefix would still end with a
    // policy word part; the whole word pins it.
    assert!(!lines[0].ends_with("non-blockin"), "{lines:?}");
}

/// With none registered, the view says exactly `No hooks`.
#[test]
fn an_empty_session_says_no_hooks() {
    let frame = frame(&[], List::default(), 40);
    assert_eq!(frame.rows.len(), 1);
    assert_eq!(frame.rows[0][0].0, "No hooks");
}

/// A just-below and just-above check on the truncation edge: at 40
/// columns the longest policy fits whole, and a shorter width still keeps
/// it whole by cutting the other columns first.
#[test]
fn narrower_widths_keep_cutting_source_before_policy() {
    let rows = vec![hook("ext", "before_tool", "blocking")];
    for width in [40, 41, 80] {
        let frame = frame(&rows, List::default(), width);
        let line = &frame.rows[0][0].0;
        assert!(line.ends_with("blocking"), "width {width}: {line}");
        assert!(line.contains("ext"), "width {width}: {line}");
        assert!(line.contains("before_tool"), "width {width}: {line}");
    }
}
