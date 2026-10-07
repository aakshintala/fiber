//! Tests for the loop's mouse handling: clicks on the targets drawn, and
//! hover's bytes.

use super::Input;
use super::tests::{Sink, feed, new_loop, offering};
use crate::link::Line;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// A left click at 0-based `col`, `row`: the press and the release.
fn click(col: u16, row: u16) -> Input {
    let (col, row) = (col + 1, row + 1);
    Input::Bytes(format!("\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m").into_bytes())
}

/// A motion report at 0-based `col`, `row`.
fn motion(col: u16, row: u16) -> Input {
    Input::Bytes(format!("\x1b[<35;{};{}M", col + 1, row + 1).into_bytes())
}

/// Esc, which puts the request shown aside, so the badge shows on row 10
/// of the 60x12 screen, at columns 0 to 29.
fn esc() -> Input {
    Input::Bytes(b"\x1b".to_vec())
}

#[test]
fn a_click_on_the_badge_reopens_the_approval_queue() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    feed(&mut lp, vec![offering("r_1"), esc()]);
    assert!(lp.app.panel().is_none());
    // A click beside the badge does nothing.
    feed(&mut lp, vec![click(30, 10)]);
    assert!(lp.app.panel().is_none());
    feed(&mut lp, vec![click(29, 10)]);
    assert!(lp.app.panel().is_some());
}

#[test]
fn a_press_and_release_split_across_reads_still_click() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    feed(&mut lp, vec![offering("r_1"), esc()]);
    feed(
        &mut lp,
        vec![
            Input::Bytes(b"\x1b[<0;1;11M".to_vec()),
            Input::Bytes(b"\x1b[<0;2;11m".to_vec()),
        ],
    );
    assert!(lp.app.panel().is_some());
}

/// One envelope of session `s_aaaaaaaaaaaaaaaa`.
fn session(kind: &str, payload: serde_json::Value, action: Option<&str>) -> Input {
    Input::Hub(Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }))
}

#[test]
fn a_click_on_new_messages_below_jumps_to_the_end() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.app
        .attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let started = serde_json::json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "hi"}]}]});
    feed(
        &mut lp,
        vec![
            session("turn_started", started, None),
            Input::Bytes(b"\x1b[5~".to_vec()),
            session(
                "assistant_message_delta",
                serde_json::json!({"text": "Hello."}),
                Some("a_1"),
            ),
        ],
    );
    assert!(lp.app.has_new());
    // The overlay's 20 cells are centred on row 10: columns 20 to 39.
    feed(&mut lp, vec![click(19, 10)]);
    assert!(lp.app.has_new());
    feed(&mut lp, vec![click(39, 10)]);
    assert!(!lp.app.has_new());
    assert_eq!(lp.app.top(), None);
}

#[test]
fn hover_writes_only_when_the_target_under_the_pointer_changes() {
    let sink = Sink::default();
    let (mut lp, _) = new_loop(CrosstermBackend::new(sink.clone()), None);
    feed(&mut lp, vec![offering("r_1"), esc()]);
    let mut written = sink.len();
    let mut wrote = |lp: &mut super::Loop<_>, input: Input| {
        feed(lp, vec![input]);
        let now = sink.len();
        let bytes = now - written;
        written = now;
        bytes
    };
    assert_eq!(wrote(&mut lp, motion(40, 5)), 0, "off every target");
    assert!(wrote(&mut lp, motion(2, 10)) > 0, "onto the badge");
    assert_eq!(wrote(&mut lp, motion(2, 10)), 0, "the same cell");
    assert_eq!(wrote(&mut lp, motion(29, 10)), 0, "the same target");
    assert!(wrote(&mut lp, motion(30, 10)) > 0, "off the badge");
    assert_eq!(wrote(&mut lp, motion(31, 10)), 0, "still off");
}

#[test]
fn hover_redraws_only_the_targets_row() {
    let (mut lp, _) = new_loop(Cells::default(), None);
    feed(&mut lp, vec![offering("r_1"), esc()]);
    lp.screen.terminal.backend_mut().inner.drawn.clear();
    feed(&mut lp, vec![motion(2, 10)]);
    let drawn = &lp.screen.terminal.backend().inner.drawn;
    assert_eq!(drawn.len(), 30);
    assert!(drawn.iter().all(|&(x, y)| y == 10 && x < 30), "{drawn:?}");
}

#[test]
fn with_hover_off_motion_writes_nothing_and_clicks_still_work() {
    let sink = Sink::default();
    let (mut lp, _) = new_loop(CrosstermBackend::new(sink.clone()), None);
    lp.hover = false;
    feed(&mut lp, vec![offering("r_1"), esc()]);
    let before = sink.len();
    feed(&mut lp, vec![motion(2, 10)]);
    assert_eq!(sink.len(), before);
    assert_eq!(lp.pointer.at, None);
    feed(&mut lp, vec![click(2, 10)]);
    assert!(lp.app.panel().is_some());
}

/// A test backend that records each cell it is asked to draw.
#[derive(Default)]
struct Cells {
    /// Every cell drawn, as column and row.
    drawn: Vec<(u16, u16)>,
}

impl Backend for Cells {
    type Error = std::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.drawn.extend(content.map(|(x, y, _)| (x, y)));
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        Ok(Position::new(0, 0))
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, _: P) -> Result<(), Self::Error> {
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn clear_region(&mut self, _: ClearType) -> Result<(), Self::Error> {
        Ok(())
    }

    fn size(&self) -> Result<Size, Self::Error> {
        Ok(Size::new(60, 12))
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        Ok(WindowSize {
            columns_rows: Size::new(60, 12),
            pixels: Size::default(),
        })
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[test]
fn hover_frames_counts_the_bytes_each_report_wrote() {
    let events = include_str!("../examples/hover.jsonl");
    // The request is put aside: the badge is on row 10 of 60x12.
    let bytes = crate::hover_frames(events, 60, 12, &[(3, 10), (4, 10), (3, 0), (3, 0)])
        .unwrap_or_else(|error| panic!("hover_frames: {error}"));
    assert_eq!(bytes.len(), 4);
    assert!(bytes[0] > 0);
    assert_eq!(bytes[1], 0);
    assert!(bytes[2] > 0);
    assert_eq!(bytes[3], 0);
    assert_eq!(
        crate::hover_frames("not json", 60, 12, &[]).map_err(|e| e.starts_with("line 1:")),
        Err(true)
    );
}

/// The screen a loop on a [`TestBackend`] shows, as text.
fn shown(lp: &super::Loop<TestBackend>) -> String {
    crate::view::text(lp.screen.terminal.backend().inner.buffer())
}

/// The first row of `lp`'s screen whose text, trimmed, starts with `start`.
fn row_of(lp: &super::Loop<TestBackend>, start: &str) -> u16 {
    shown(lp)
        .lines()
        .position(|row| row.trim_start().starts_with(start))
        .and_then(|at| u16::try_from(at).ok())
        .unwrap_or_else(|| panic!("no row starts {start:?} in\n{}", shown(lp)))
}

/// A completed turn that thought, then read one file.
fn thought_and_read() -> Vec<Input> {
    use serde_json::json;
    let started = json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "hi"}]}]});
    vec![
        session("turn_started", started, None),
        session("step_started", json!({}), None),
        session("reasoning_started", json!({}), Some("a_t")),
        session(
            "reasoning_completed",
            json!({"text": "# Plan\nRead a.rs first."}),
            Some("a_t"),
        ),
        session(
            "tool_call_requested",
            json!({"name": "read", "arguments": {"path": "src/a.rs"}}),
            Some("a_1"),
        ),
        session(
            "tool_call_completed",
            json!({"status": "completed",
                "content": [{"type": "text", "text": "fn body_of_a() {}"}]}),
            Some("a_1"),
        ),
        session("turn_completed", json!({"outcome": "completed"}), None),
    ]
}

/// A loop showing [`thought_and_read`] on `backend`.
fn grouped<B: ratatui::backend::Backend>(backend: B) -> super::Loop<B> {
    let (mut lp, _) = new_loop(backend, None);
    lp.app
        .attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    feed(&mut lp, thought_and_read());
    lp
}

#[test]
fn clicks_open_the_ledger_then_a_call_then_the_thought() {
    let mut lp = grouped(TestBackend::new(60, 12));
    assert!(!shown(&lp).contains("read src/a.rs"), "{}", shown(&lp));
    let group = row_of(&lp, "• Read");
    feed(&mut lp, vec![click(40, group)]);
    let read = row_of(&lp, "read src/a.rs");
    assert!(!shown(&lp).contains("fn body_of_a"), "{}", shown(&lp));
    feed(&mut lp, vec![click(50, read)]);
    assert!(shown(&lp).contains("fn body_of_a"), "{}", shown(&lp));
    let thought = row_of(&lp, "1 + Thought");
    assert!(!shown(&lp).contains("Read a.rs first."), "{}", shown(&lp));
    feed(&mut lp, vec![click(0, thought)]);
    assert!(shown(&lp).contains("Read a.rs first."), "{}", shown(&lp));
    // A second click on the summary closes the ledger again.
    let group = row_of(&lp, "• Read");
    feed(&mut lp, vec![click(0, group)]);
    assert!(!shown(&lp).contains("read src/a.rs"), "{}", shown(&lp));
}

#[test]
fn hover_over_a_group_line_tints_only_its_row() {
    let lp = grouped(TestBackend::new(60, 12));
    let group = row_of(&lp, "• Read");
    let mut lp = grouped(Cells::default());
    lp.screen.terminal.backend_mut().inner.drawn.clear();
    feed(&mut lp, vec![motion(5, group)]);
    let drawn = &lp.screen.terminal.backend().inner.drawn;
    assert_eq!(drawn.len(), 60);
    assert!(drawn.iter().all(|&(_, y)| y == group), "{drawn:?}");
    let mut lp = grouped(TestBackend::new(60, 12));
    feed(&mut lp, vec![motion(5, group)]);
    let buf = lp.screen.terminal.backend().inner.buffer();
    for y in 0..12 {
        for x in 0..60 {
            let bg = buf.cell((x, y)).map(|cell| cell.bg);
            let tinted = bg == crate::view::HOVER_TINT.bg;
            assert_eq!(tinted, y == group, "cell {x},{y}");
        }
    }
}

/// An attached loop at 60x12 fed `inputs`.
fn attached(inputs: Vec<Input>) -> super::Loop<TestBackend> {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.app
        .attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    feed(&mut lp, inputs);
    lp
}

/// The click target under 0-based `col`, `row` of `lp`'s last frame.
fn hit_at(lp: &super::Loop<TestBackend>, col: u16, row: u16) -> Option<crate::mouse::TargetId> {
    crate::mouse::hit(&lp.screen.targets, col, row)
}

#[test]
fn a_click_on_a_steering_row_selects_it_into_the_draft() {
    use serde_json::json;
    let mut lp = attached(vec![
        Input::Bytes(b"mine".to_vec()),
        session(
            "steering_queue",
            json!({"messages": [
                {"content": [{"type": "text", "text": "use the parser"}], "source": "driver", "command_id": "c_1"},
                {"content": [{"type": "text", "text": "and test it"}], "source": "driver", "command_id": "c_2"},
            ]}),
            None,
        ),
    ]);
    // The queue's rows sit on rows 9 and 10, above the input line.
    assert_eq!(row_of(&lp, "↳ use the parser"), 9);
    assert_eq!(row_of(&lp, "↳ and test it"), 10);
    // Beside a row's text is no target.
    feed(&mut lp, vec![click(40, 9)]);
    assert_eq!(lp.app.input().expand(), "mine");
    feed(&mut lp, vec![click(3, 9)]);
    assert_eq!(lp.app.input().expand(), "use the parser");
    assert_eq!(row_of(&lp, "▸ use the parser"), 9);
    // Esc puts the stashed draft back.
    feed(&mut lp, vec![esc()]);
    assert_eq!(lp.app.input().expand(), "mine");
}

/// `n` notices from the attached session, "Notice 1." oldest.
fn notices(n: usize) -> Vec<Input> {
    (1..=n)
        .map(|at| {
            session(
                "notice",
                serde_json::json!({"code": "extension_failed", "message": format!("Notice {at}.")}),
                None,
            )
        })
        .collect()
}

#[test]
fn a_click_on_a_notice_shows_it_whole_and_its_cross_dismisses_it() {
    let mut lp = attached(notices(2));
    // Each box is 24 columns at the right, newest on top: its ✕ at 59.
    assert!(
        shown(&lp)
            .lines()
            .next()
            .is_some_and(|row| row.contains("Notice 2."))
    );
    // Left of the box is the conversation, no target.
    assert_eq!(hit_at(&lp, 35, 0), None);
    feed(&mut lp, vec![click(36, 0)]);
    assert_eq!(lp.app.notice_overlay(), Some(vec!["Notice 2.".to_owned()]));
    feed(&mut lp, vec![esc()]);
    assert_eq!(lp.app.notice_overlay(), None);
    feed(&mut lp, vec![click(59, 0)]);
    assert!(!shown(&lp).contains("Notice 2."), "{}", shown(&lp));
    assert!(
        shown(&lp)
            .lines()
            .next()
            .is_some_and(|row| row.contains("Notice 1."))
    );
    assert_eq!(lp.app.notice_overlay(), None);
}

#[test]
fn a_click_on_more_lists_every_notice() {
    let mut lp = attached(notices(4));
    let more = row_of(&lp, "+1 more");
    assert_eq!(more, 3);
    feed(&mut lp, vec![click(40, more)]);
    let listed: Vec<String> = (1..=4).rev().map(|at| format!("Notice {at}.")).collect();
    assert_eq!(lp.app.notice_overlay(), Some(listed));
}

#[test]
fn the_open_notice_overlay_hides_the_conversation_targets() {
    let mut lp = grouped(TestBackend::new(60, 12));
    let group = row_of(&lp, "• Read");
    feed(&mut lp, notices(1));
    feed(&mut lp, vec![click(36, 0)]);
    assert!(lp.app.notice_overlay().is_some());
    assert_eq!(hit_at(&lp, 0, group), None);
}

#[test]
fn a_click_on_a_failed_logins_line_hits_log_in() {
    use crate::app::Target;
    use crate::mouse::TargetId;
    use serde_json::json;
    let started = json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    let failed = json!({"outcome": "failed", "error": {"code": "authentication_failed",
        "message": "The key was refused."}});
    let mut lp = attached(vec![
        session("turn_started", started, None),
        session("turn_completed", failed, None),
    ]);
    let line = row_of(&lp, "✗ The key was refused.");
    assert_eq!(hit_at(&lp, 0, line), Some(TargetId::Line(Target::Login)));
    // The login view is #678's; the click changes nothing yet.
    let before = shown(&lp);
    feed(&mut lp, vec![click(0, line)]);
    assert_eq!(shown(&lp), before);
}

#[test]
fn a_click_on_a_handoffs_note_line_opens_the_note() {
    use serde_json::json;
    let preamble = json!({"reason": "start", "model": "fake/m", "context_window": 1_000_000,
        "tool_choice": "auto", "cache_lifetime": "5m", "system_prompt": "", "tools": [],
        "trigger_at": 400_000});
    let started = json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    let mut lp = attached(vec![
        session("preamble_built", preamble, None),
        session("turn_started", started, None),
        session("handoff_started", json!({"trigger": "auto"}), None),
        session(
            "text_completed",
            json!({"text": "## Note\nkeep going"}),
            Some("a_n"),
        ),
        session(
            "handoff_completed",
            json!({"outcome": "completed", "note": ["a_n"], "tokens_before": 402_000}),
            None,
        ),
        session(
            "assistant_message_delta",
            json!({"text": "Continuing."}),
            Some("a_2"),
        ),
        session("turn_completed", json!({"outcome": "completed"}), None),
    ]);
    assert!(!shown(&lp).contains("keep going"), "{}", shown(&lp));
    let note = row_of(&lp, "▸ note");
    feed(&mut lp, vec![click(4, note)]);
    assert!(shown(&lp).contains("keep going"), "{}", shown(&lp));
    // The opened note pushes the bottom-aligned rows up.
    let note = row_of(&lp, "▸ note");
    feed(&mut lp, vec![click(4, note)]);
    assert!(!shown(&lp).contains("keep going"), "{}", shown(&lp));
}

#[test]
fn a_click_on_the_orphaned_jobs_line_shows_each_jobs_message() {
    use serde_json::json;
    let started = json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "go"}]}]});
    let mut lp = attached(vec![
        session(
            "job_started",
            json!({"job_id": "j_1", "description": "build the docs", "output_path": "/tmp/o"}),
            None,
        ),
        session("turn_started", started, None),
        session(
            "fiber_started",
            json!({"version": "0.0.1", "resumed": true}),
            None,
        ),
        session(
            "job_completed",
            json!({"job_id": "j_1", "status": "failed",
                "error": {"code": "orphaned", "message": "j_1 orphaned."}}),
            None,
        ),
    ]);
    assert!(!shown(&lp).contains("j_1 orphaned."), "{}", shown(&lp));
    let line = row_of(&lp, "Orphaned jobs: build the docs");
    feed(&mut lp, vec![click(0, line)]);
    assert!(
        shown(&lp).contains("build the docs: j_1 orphaned."),
        "{}",
        shown(&lp)
    );
}
