//! Tests for `/logout`'s wheel window (`logout_view.rs`): the wheel
//! shifts the provider list three rows a step, keeping the focus while
//! it shows, and the confirm takes no wheel.

use super::Logout;
use crate::configure::{LoginKind, LoginTarget};
use crate::configure_fake::Fake;
use crate::keys::Key;
use crate::settings_view::{Act, Ctx};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::path::Path;

/// Thirty key providers: enough rows that the window moves three and
/// the focus stays on screen.
fn many_targets() -> Vec<LoginTarget> {
    (0..30)
        .map(|at| LoginTarget {
            name: format!("p{at:02}"),
            kind: LoginKind::Key,
        })
        .collect()
}

/// A seam answering `targets`.
fn fake_with(targets: Vec<LoginTarget>) -> Fake {
    let fake = Fake::new(Vec::new());
    if let Ok(mut held) = fake.logout.lock() {
        *held = Ok(targets
            .into_iter()
            .enumerate()
            .map(|(at, target)| crate::configure::LogoutRow::Provider {
                name: target.name,
                label: format!("l{at:02}"),
                default: at == 0,
                deletable: true,
                origin: None,
                siblings: Vec::new(),
            })
            .collect());
    }
    fake
}

/// A call's context over `fake`, `height` rows tall, in `/w`.
fn ctx_h(fake: &Fake, height: usize) -> Ctx<'_> {
    Ctx {
        seam: fake,
        workspace: Path::new("/w"),
        height,
        width: 80,
        usage: None,
    }
}

/// The wheel over the rows shifts the window three rows and keeps a
/// focus still on screen: five Downs put it mid-window, so one step
/// down moves the window three rows under it (`docs/tui.md:576`).
#[test]
fn wheel_keeps_a_visible_focus_and_shifts_the_window_three() {
    let fake = fake_with(many_targets());
    let context = ctx_h(&fake, 24);
    let mut logout = Logout::open(&context);
    assert_eq!(logout.selected, 1);
    for _ in 0..5 {
        logout.key(&Key::Down, &context);
    }
    assert_eq!(logout.selected, 6);
    assert_eq!(logout.top, 0);
    assert!(matches!(logout.wheel(false, &context), Act::Stay));
    assert_eq!(logout.top, 3);
    assert_eq!(
        logout.selected, 6,
        "a focus still on screen stays where it was"
    );
}

/// The wheel clamps a focus that leaves the window to the nearest
/// shown target row: from the top row, one step down moves the window
/// three rows and the focus to its first row.
/// (`docs/tui.md:576`).
#[test]
fn wheel_clamps_a_leaving_focus_to_the_window() {
    let fake = fake_with(many_targets());
    let context = ctx_h(&fake, 24);
    let mut logout = Logout::open(&context);
    assert_eq!(logout.selected, 1);
    assert!(matches!(logout.wheel(false, &context), Act::Stay));
    assert_eq!(logout.top, 3);
    assert_eq!(
        logout.selected, 3,
        "the focus clamps to the nearest shown target row"
    );
}

/// Ordinary arrow keys still move one target after wheel movement, and
/// the wheeled top survives while both targets remain visible.
#[test]
fn keyboard_navigation_after_wheel_keeps_its_one_target_step() {
    let fake = fake_with(many_targets());
    let context = ctx_h(&fake, 24);
    let mut logout = Logout::open(&context);
    logout.wheel(false, &context);
    assert_eq!((logout.top, logout.selected), (3, 3));
    logout.key(&Key::Down, &context);
    assert_eq!((logout.top, logout.selected), (3, 4));
    logout.key(&Key::Up, &context);
    assert_eq!((logout.top, logout.selected), (3, 3));
}

/// The wheel up from the top row moves nothing, and the confirm takes
/// no wheel: Enter asks, and the wheel over it changes nothing
/// (`docs/tui.md:576`).
#[test]
fn wheel_up_at_the_top_and_over_the_confirm_moves_nothing() {
    let fake = fake_with(many_targets());
    let context = ctx_h(&fake, 24);
    let mut logout = Logout::open(&context);
    assert!(matches!(logout.wheel(true, &context), Act::Stay));
    assert_eq!((logout.top, logout.selected), (0, 1));
    for _ in 0..3 {
        logout.key(&Key::Down, &context);
    }
    assert_eq!(logout.selected, 4);
    assert!(matches!(logout.key(&Key::Enter, &context), Act::Stay));
    assert!(matches!(logout.wheel(false, &context), Act::Stay));
    assert!(matches!(logout.wheel(true, &context), Act::Stay));
    assert_eq!(
        logout.selected, 4,
        "the wheel over the confirm keeps the confirmed row"
    );
}

/// The wheel after keyboard focus leaves the stored window steps three
/// rows from what is visible, not the stale stored top (`docs/tui.md:576`).
#[test]
fn wheel_after_keyboard_outside_the_window_steps_from_what_is_visible() {
    let fake = fake_with(many_targets());
    let context = ctx_h(&fake, 12);
    let mut logout = Logout::open(&context);
    for _ in 0..15 {
        logout.key(&Key::Down, &context);
    }
    let (stored, selected) = (logout.top, logout.selected);
    assert_eq!(stored, 0, "the keys never move the stored top");
    let shown = super::shown_body_rows(12);
    let rows = logout.body_len(80);
    let end = rows.saturating_sub(shown);
    let low = selected.saturating_add(1).saturating_sub(shown);
    let rendered = stored.clamp(low, selected).min(end);
    assert_ne!(rendered, stored, "the focus left the stored window");
    assert!(matches!(logout.wheel(false, &context), Act::Stay));
    assert_eq!(
        logout.top,
        rendered.saturating_add(3).min(end),
        "one step down moves three rows from what was visible"
    );
}

/// The body length counts wrapped status rows, and a short draw clips
/// safely (`docs/tui.md:576`).
#[test]
fn body_len_counts_wrapped_status_and_short_draw_clips() {
    let fake = fake_with(many_targets());
    let context = ctx_h(&fake, 24);
    let mut logout = Logout::open(&context);
    let plain = logout.body_len(80);
    logout.said = vec!["x".repeat(200)];
    let wrapped = logout.body_len(80);
    assert!(
        wrapped > plain + 1,
        "a wrapped status adds rows: {plain} vs {wrapped}"
    );
    logout.said.clear();
    assert_eq!(logout.body_len(80), plain, "clearing restores the count");
    let area = Rect::new(0, 0, 80, 2);
    let mut buf = Buffer::empty(area);
    let mut targets = Vec::new();
    let _ = logout.draw(area, &mut buf, &mut targets);
}
