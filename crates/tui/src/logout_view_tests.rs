//! Unit tests for `/logout`'s window arithmetic (`logout_view.rs`): the
//! edge table no screen test can separate, and the scrolled starts a
//! tall list shows.

use super::window_start;

#[test]
fn window_start_edges_empty_body_and_empty_window_at_the_top() {
    // An empty body or window shows nothing: the window starts at the
    // top whatever holds the focus.
    for (focused, shown, rows) in [
        (0, 0, 0),
        (3, 0, 5),
        (0, 0, 5),
        (3, 5, 0),
        (0, 5, 0),
        (0, 16, 31),
        (15, 16, 31),
    ] {
        assert_eq!(
            window_start(focused, shown, rows),
            0,
            "focused={focused} shown={shown} rows={rows}"
        );
    }
    // Past the window the start keeps the focused row as the last shown.
    assert_eq!(window_start(16, 16, 31), 1);
    assert_eq!(window_start(30, 16, 31), 15);
}
