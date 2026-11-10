//! Tests for the extension click route at the app level: a press and
//! release paired on one extension target dispatches once, and only an
//! unhandled click reaches the built-in target under it.

use super::super::{App, Effect};
use crate::app::panel::Spot;
use crate::app::testkit::{self, SESSION, attached};
use crate::ext::{Decision, KeyOutcome, Renderer, SessionCtx};
use crate::keys::{Button, Mouse, MouseKind};
use crate::mouse::{Pointer, Target as Hit, TargetId};
use contract::SessionId;
use ratatui::layout::Rect;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// A canned click renderer: records the session, slot and id each
/// `on_click` call sees, answers with its outcome, and queues its
/// pending notices for the route to drain.
#[derive(Debug, Default)]
struct Clicked {
    calls: RefCell<Vec<(Option<String>, String, String)>>,
    handled: Cell<bool>,
    pending: RefCell<Vec<(String, String)>>,
}

impl Clicked {
    /// A canned renderer answering `handled`.
    fn canned(handled: bool) -> Rc<Self> {
        Rc::new(Clicked {
            handled: Cell::new(handled),
            ..Clicked::default()
        })
    }
}

impl Renderer for Clicked {
    fn name(&self) -> &str {
        "clicked"
    }

    fn fills(&self, _slot: &str) -> bool {
        false
    }

    fn render(
        &self,
        _ctx: &SessionCtx,
        _slot: &str,
        _input: &crate::ext::Input<'_>,
        _width: u16,
        _builtin: &dyn Fn(&crate::ext::Input<'_>, u16) -> Vec<Vec<crate::ext::RawSpan>>,
    ) -> Decision {
        Decision::Draw {
            extension: "clicked".into(),
            lines: Vec::new(),
        }
    }

    fn on_click(&self, ctx: &SessionCtx, slot: &str, id: &str) -> KeyOutcome {
        self.calls.borrow_mut().push((
            ctx.session_id.clone().map(|id| id.0),
            slot.to_owned(),
            id.to_owned(),
        ));
        if self.handled.get() {
            KeyOutcome::Handled
        } else {
            KeyOutcome::NotHandled
        }
    }

    fn take_pending(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.pending.borrow_mut())
    }
}

/// An app with `clicked` installed, and the click key its own cache
/// interned for the (`clicked`, `notice`, `badge`) tuple.
fn clicked_app(clicked: &Rc<Clicked>) -> (App, crate::ext::cache::ClickKey) {
    let mut app = attached(80, 24);
    app.install_renderer(clicked.clone());
    let key = app.screen.pages().ext.cache.intern_click(
        &SessionId(SESSION.to_owned()),
        "clicked",
        "notice",
        "badge",
    );
    (app, key)
}

/// A jobs panel target under an extension target sharing its cells:
/// the fallback toggles the jobs panel, which a consumed click never
/// reaches.
fn targets(key: crate::ext::cache::ClickKey) -> Vec<Hit> {
    vec![
        Hit {
            id: TargetId::Panel(Spot::Jobs),
            rect: Rect::new(0, 0, 10, 1),
        },
        Hit {
            id: TargetId::Extension(key),
            rect: Rect::new(0, 0, 4, 1),
        },
    ]
}

/// A press at the extension's cells and its release on them, through
/// the frame's one target list.
fn click(app: &mut App, targets: &[Hit]) -> Effect {
    click_at(app, targets, 0, 0, 3, 0)
}

/// A press at (`press_col`, `press_row`) and its release at
/// (`release_col`, `release_row`), through the frame's one target list.
fn click_at(
    app: &mut App,
    targets: &[Hit],
    press_col: u16,
    press_row: u16,
    release_col: u16,
    release_row: u16,
) -> Effect {
    let press = Mouse {
        kind: MouseKind::Press(Button::Left),
        col: press_col,
        row: press_row,
    };
    let release = Mouse {
        kind: MouseKind::Release,
        col: release_col,
        row: release_row,
    };
    let now = testkit::now();
    let mut pointer = Pointer::default();
    app.on_mouse_inner(&press, targets, &mut pointer, true, now);
    app.on_mouse_inner(&release, targets, &mut pointer, true, now)
}

#[test]
fn click_handled_consumes_the_route() {
    let clicked = Clicked::canned(true);
    clicked
        .pending
        .borrow_mut()
        .push(("hook_failed".into(), "clicked failed loudly".to_owned()));
    let (mut app, key) = clicked_app(&clicked);
    let targets = targets(key);
    assert_eq!(click(&mut app, &targets), Effect::None);
    // The handler saw the session, slot and id; its notice drained to
    // the screen; the fallback never ran.
    assert_eq!(
        clicked.calls.borrow().clone(),
        vec![(
            Some(SESSION.to_owned()),
            "notice".to_owned(),
            "badge".to_owned()
        )]
    );
    assert_eq!(app.notice(), Some("clicked failed loudly"));
    assert!(!app.panel_state.jobs_open());
}

#[test]
fn click_unhandled_resumes_the_fallback() {
    let clicked = Clicked::canned(false);
    clicked
        .pending
        .borrow_mut()
        .push(("hook_failed".into(), "clicked declined loudly".to_owned()));
    let (mut app, key) = clicked_app(&clicked);
    let targets = targets(key);
    assert_eq!(click(&mut app, &targets), Effect::None);
    // The handler ran and its notice drained; the built-in target under
    // the extension ran too.
    assert_eq!(clicked.calls.borrow().len(), 1);
    assert_eq!(app.notice(), Some("clicked declined loudly"));
    assert!(app.panel_state.jobs_open());
}

#[test]
fn click_resolves_the_route_at_the_extension_stop() {
    let clicked = Clicked::canned(true);
    let (mut app, key) = clicked_app(&clicked);
    // The extension stop and the panel sit on disjoint cells: the route
    // must resolve at the extension stop's own cells, never a guessed
    // coordinate (`docs/tui.md`, "Input and focus").
    app.drawn(&[
        Hit {
            id: TargetId::Extension(key),
            rect: Rect::new(0, 0, 4, 1),
        },
        Hit {
            id: TargetId::Panel(Spot::Jobs),
            rect: Rect::new(5, 0, 5, 1),
        },
    ]);
    app.on_click(TargetId::Extension(key));
    // The handler saw the session, slot and id; the panel stayed shut.
    assert_eq!(
        clicked.calls.borrow().clone(),
        vec![(
            Some(SESSION.to_owned()),
            "notice".to_owned(),
            "badge".to_owned()
        )]
    );
    assert!(!app.panel_state.jobs_open());
}

#[test]
fn click_stale_key_never_reaches_the_handler() {
    let clicked = Clicked::canned(true);
    let (mut app, _) = clicked_app(&clicked);
    // A key another cache minted names nothing in this app's lookup:
    // the scratch cache's second key is past every key this app holds.
    let scratch = crate::ext::cache::Cache::default();
    scratch.intern_click(&SessionId(SESSION.to_owned()), "gone", "notice", "badge");
    let stale = scratch.intern_click(&SessionId(SESSION.to_owned()), "gone", "notice", "other");
    let targets = targets(stale);
    assert_eq!(click(&mut app, &targets), Effect::None);
    // No dispatch, so the queued notice stays queued and the fallback
    // runs.
    assert!(clicked.calls.borrow().is_empty());
    assert_eq!(app.notice(), None);
    assert!(app.panel_state.jobs_open());
}

#[test]
fn click_without_a_renderer_resumes_the_fallback() {
    let mut app = attached(80, 24);
    let key = app.screen.pages().ext.cache.intern_click(
        &SessionId(SESSION.to_owned()),
        "clicked",
        "notice",
        "badge",
    );
    assert_eq!(click(&mut app, &targets(key)), Effect::None);
    assert!(app.panel_state.jobs_open());
}

#[test]
fn click_home_carries_no_session() {
    let clicked = Clicked::canned(true);
    let mut app = App::new(std::path::PathBuf::from("/w"));
    app.set_size(80, 24);
    app.install_renderer(clicked.clone());
    let key = app.screen.pages().ext.cache.intern_click(
        &SessionId(String::new()),
        "clicked",
        "notice",
        "badge",
    );
    assert_eq!(click(&mut app, &targets(key)), Effect::None);
    // Home has no session: the handler runs with the field omitted, and
    // the fallback never runs on a handled click.
    assert_eq!(
        clicked.calls.borrow().clone(),
        vec![(None, "notice".to_owned(), "badge".to_owned())]
    );
    assert!(!app.panel_state.jobs_open());
}

/// A declined or errored click preserves its built-in target: over
/// real line, notice and interrupt candidates the observable outcome
/// matches the no-extension baseline, and over the jobs panel the
/// panel still opens while the error notice drains.
#[test]
fn click_false_and_error_preserve_builtin() {
    use crate::app::Target;
    // One row per built-in candidate; the extension spans them all.
    // Row 0 opens or closes a group line, row 1 a notice box, row 2
    // the interrupt stop, row 3 the jobs panel.
    let candidates = || {
        vec![
            Hit {
                id: TargetId::Line(Target::Group(7)),
                rect: Rect::new(0, 0, 4, 1),
            },
            Hit {
                id: TargetId::Notice(0),
                rect: Rect::new(0, 1, 4, 1),
            },
            Hit {
                id: TargetId::Interrupt,
                rect: Rect::new(0, 2, 4, 1),
            },
            Hit {
                id: TargetId::Panel(Spot::Jobs),
                rect: Rect::new(0, 3, 4, 1),
            },
        ]
    };
    let over = |key: crate::ext::cache::ClickKey| {
        let mut with = candidates();
        with.push(Hit {
            id: TargetId::Extension(key),
            rect: Rect::new(0, 0, 4, 4),
        });
        with
    };
    // The no-extension baseline for a click on each row.
    let baseline = |row: u16| {
        let mut app = attached(80, 24);
        app.push_notice(contract::ErrorCode::SkillShadowed, "kept notice".to_owned());
        assert_eq!(app.notices.live_ids(), vec![0]);
        let effect = click_at(&mut app, &candidates(), 0, row, 3, row);
        (effect, app.panel_state.jobs_open(), app.notices.live_ids())
    };
    // A declined click over the same rows: the handler ran, and every
    // observable matches its baseline.
    let declined = |row: u16| {
        let clicked = Clicked::canned(false);
        let (mut app, key) = clicked_app(&clicked);
        app.push_notice(contract::ErrorCode::SkillShadowed, "kept notice".to_owned());
        let effect = click_at(&mut app, &over(key), 0, row, 3, row);
        assert_eq!(clicked.calls.borrow().len(), 1, "row {row}");
        (effect, app.panel_state.jobs_open(), app.notices.live_ids())
    };
    for row in 0..4 {
        assert_eq!(declined(row), baseline(row), "row {row}");
    }
    // An errored click over the panel still opens it while its notice
    // drains.
    let clicked = Clicked::canned(false);
    clicked.pending.borrow_mut().push((
        "extension_failed".into(),
        "clicked passed its timeout".to_owned(),
    ));
    let (mut app, key) = clicked_app(&clicked);
    let effect = click_at(&mut app, &over(key), 0, 3, 3, 3);
    assert_eq!(effect, Effect::None);
    assert_eq!(clicked.calls.borrow().len(), 1);
    assert_eq!(app.notice(), Some("clicked passed its timeout"));
    assert!(app.panel_state.jobs_open());
}

/// A wheel scrolls the conversation and a drag selects and copies it:
/// both stay terminal gestures when they start on an extension target,
/// and a handled click runs no built-in mutation.
#[test]
fn click_wheel_and_drag_stay_terminal() {
    let clicked = Clicked::canned(true);
    let (mut app, key) = clicked_app(&clicked);
    // Thirty one-line turns outgrow the window, so a wheel up from the
    // followed bottom moves the top.
    testkit::long_turns(&mut app, &mut 0, 30);
    assert_eq!(app.top(), None);
    let targets = targets(key);
    let now = testkit::now();
    let mut pointer = Pointer::default();
    // A wheel over the extension target scrolls instead of clicking:
    // the top moves, with no handler call and no fallback toggle.
    let wheel = Mouse {
        kind: MouseKind::WheelUp,
        col: 1,
        row: 0,
    };
    assert_eq!(
        app.on_mouse_inner(&wheel, &targets, &mut pointer, true, now),
        Effect::None
    );
    assert_ne!(app.top(), None, "the wheel scrolled");
    assert!(clicked.calls.borrow().is_empty());
    assert!(!app.panel_state.jobs_open());
    // A drag from the extension target selects instead of clicking: the
    // release copies the drawn text and the handler never runs. The
    // press sits inside the conversation area, past the gutter, on the
    // extension's cells.
    let press = Mouse {
        kind: MouseKind::Press(Button::Left),
        col: 3,
        row: 0,
    };
    let drag = Mouse {
        kind: MouseKind::Drag(Button::Left),
        col: 5,
        row: 0,
    };
    let release = Mouse {
        kind: MouseKind::Release,
        col: 5,
        row: 0,
    };
    app.on_mouse_inner(&press, &targets, &mut pointer, true, now);
    app.on_mouse_inner(&drag, &targets, &mut pointer, true, now);
    let effect = app.on_mouse_inner(&release, &targets, &mut pointer, true, now);
    assert!(matches!(effect, Effect::Copy(_)), "{effect:?}");
    assert!(app.select.span().is_some(), "a selection");
    assert!(clicked.calls.borrow().is_empty());
    assert!(!app.panel_state.jobs_open());
}

#[test]
fn click_error_notice_drains_and_falls_through() {
    let clicked = Clicked::canned(false);
    clicked.pending.borrow_mut().push((
        "extension_failed".into(),
        "clicked passed its timeout".to_owned(),
    ));
    let (mut app, key) = clicked_app(&clicked);
    let targets = targets(key);
    assert_eq!(click(&mut app, &targets), Effect::None);
    assert_eq!(clicked.calls.borrow().len(), 1);
    assert_eq!(app.notice(), Some("clicked passed its timeout"));
    assert!(app.panel_state.jobs_open());
}
