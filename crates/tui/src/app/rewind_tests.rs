//! Tests for `/rewind` (#680): the view listing the conversation's turns
//! back through the `forked_from` chain, the step boundaries each turn
//! opens into, and the rewind the person sends from the chosen point.

use crate::app::Effect;
use crate::app::commands::commands_testkit::{attached, enter};

#[test]
fn rewind_opens_an_empty_view() {
    let mut app = attached();
    assert_eq!(enter(&mut app, "/rewind"), Effect::None);
    assert_eq!(app.draft(), "");
    let frame = app.session_view_screen(80).expect("the view is open");
    assert_eq!(frame.title, "Rewind");
    assert_eq!(frame.command, "/rewind");
    assert!(frame.rows.is_empty());
}
