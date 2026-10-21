//! Tests for `/diff` (#3013): it opens the Changed files view the
//! panel's card opens.

use super::commands_testkit::{attached, connected, enter, table_lines};
use crate::app::Effect;

#[test]
fn diff_opens_the_changed_files_view_and_clears_the_draft() {
    let mut app = attached();
    assert_eq!(enter(&mut app, "/diff"), Effect::None);
    assert_eq!(app.draft(), "");
    let frame = app.session_view_screen(80).expect("the view is open");
    assert_eq!(frame.title, "Changed files");
    assert_eq!(frame.command, "diff against HEAD");
}

#[test]
fn diff_with_no_session_says_so_and_opens_nothing() {
    let mut app = connected();
    assert_eq!(enter(&mut app, "/diff"), Effect::None);
    assert!(app.session_view_screen(80).is_none());
    assert_eq!(app.notice(), Some("No session on screen."));
}

#[test]
fn the_list_carries_the_diff_row() {
    assert!(
        table_lines()
            .iter()
            .any(|line| line == "/diff  Opens the Changed files view.  command")
    );
}
