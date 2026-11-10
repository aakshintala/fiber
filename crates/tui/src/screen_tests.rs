//! Tests for the screen: its viewport size and the backend that reports it.

use super::{Screen, SyncEmit, backend};
use crate::app::App;
use crate::keys::{Mouse, MouseKind};
use crate::link::Line;
use crate::look::{Look, ThemeSetting};
use crate::mouse::Target;
use crate::theme::Role;
use contract::events::EventKind;
use contract::feedback::{FrameShape, RING_DEPTH, ShapeColor, ShapeKind, StyleValue};
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};

#[test]
fn the_screen_reports_the_tty_size_not_the_backends() {
    // ratatui clears a fixed viewport at the size its backend reports;
    // the backend's own answer is never asked.
    let mut screen =
        Screen::new(TestBackend::new(60, 12), 40, 10).unwrap_or_else(|err| panic!("screen: {err}"));
    let size = |screen: &Screen<TestBackend>| {
        screen
            .terminal
            .size()
            .unwrap_or_else(|err| panic!("size: {err}"))
    };
    assert_eq!(size(&screen), ratatui::layout::Size::new(40, 10));
    screen
        .resize(30, 8)
        .unwrap_or_else(|err| panic!("resize: {err}"));
    assert_eq!(size(&screen), ratatui::layout::Size::new(30, 8));
    let window = ratatui::backend::Backend::window_size(screen.terminal.backend_mut())
        .unwrap_or_else(|err| panic!("window: {err}"));
    assert_eq!(window.columns_rows, ratatui::layout::Size::new(30, 8));
}

#[test]
fn the_screen_area_follows_the_size_it_was_given() {
    let mut screen =
        Screen::new(TestBackend::new(60, 12), 40, 10).unwrap_or_else(|err| panic!("screen: {err}"));
    assert_eq!(screen.area(), ratatui::layout::Rect::new(0, 0, 40, 10));
    screen
        .resize(30, 8)
        .unwrap_or_else(|err| panic!("resize: {err}"));
    assert_eq!(screen.area(), ratatui::layout::Rect::new(0, 0, 30, 8));
}

/// A backend that records each call it gets.
#[derive(Default)]
struct Calls(Vec<String>);

impl SyncEmit for Calls {
    fn emit(&mut self, _begin: bool) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Backend for Calls {
    type Error = std::io::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.0.push(format!("draw {}", content.count()));
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.push("hide".to_owned());
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.push("show".to_owned());
        Ok(())
    }

    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        self.0.push("get".to_owned());
        Ok(ratatui::layout::Position::new(3, 4))
    }

    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> Result<(), Self::Error> {
        let position = position.into();
        self.0.push(format!("set {} {}", position.x, position.y));
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.0.push("clear".to_owned());
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        self.0.push(format!("clear {clear_type}"));
        Ok(())
    }

    fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
        Ok(ratatui::layout::Size::new(1, 1))
    }

    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
        Ok(ratatui::backend::WindowSize {
            columns_rows: ratatui::layout::Size::new(1, 1),
            pixels: ratatui::layout::Size::new(1, 1),
        })
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.push("flush".to_owned());
        Ok(())
    }
}

#[test]
fn the_tty_sized_backend_passes_every_call_but_the_size_through() {
    let mut backend = super::TtySized {
        inner: Calls::default(),
        size: ratatui::layout::Size::new(40, 10),
        open: false,
        emit: Calls::emit,
        touched: false,
    };
    let cell = ratatui::buffer::Cell::default();
    let ok = |result: Result<(), std::io::Error>| {
        result.unwrap_or_else(|err| panic!("backend call: {err}"));
    };
    ok(backend.draw([(0, 0, &cell), (1, 0, &cell)].into_iter()));
    ok(backend.hide_cursor());
    ok(backend.show_cursor());
    let position = backend
        .get_cursor_position()
        .unwrap_or_else(|err| panic!("cursor: {err}"));
    assert_eq!(position, ratatui::layout::Position::new(3, 4));
    ok(backend.set_cursor_position((5, 6)));
    ok(backend.clear());
    ok(backend.clear_region(ratatui::backend::ClearType::CurrentLine));
    ok(backend.flush());
    assert_eq!(
        backend.inner.0,
        [
            "draw 2",
            "hide",
            "show",
            "get",
            "set 5 6",
            "clear",
            "clear CurrentLine",
            "flush"
        ]
    );
    let size = backend.size().unwrap_or_else(|err| panic!("size: {err}"));
    assert_eq!(size, ratatui::layout::Size::new(40, 10));
}

/// A writer counting each `write` call, holding every byte written.
#[derive(Clone, Default)]
struct CountingWriter {
    calls: std::rc::Rc<std::cell::Cell<usize>>,
    bytes: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
}

impl CountingWriter {
    fn calls(&self) -> usize {
        self.calls.get()
    }

    fn bytes(&self) -> Vec<u8> {
        self.bytes.borrow().clone()
    }
}

impl std::io::Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.calls.set(self.calls.get().saturating_add(1));
        self.bytes.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn frame_backend_hide_cursor_writes_the_hide_sequence() {
    let written = CountingWriter::default();
    let mut backend = backend(written.clone());
    Backend::hide_cursor(&mut backend).unwrap_or_else(|err| panic!("hide cursor: {err}"));
    assert!(
        written.bytes().is_empty(),
        "crossterm's flush stays buffered"
    );
    Backend::flush(&mut backend).unwrap_or_else(|err| panic!("flush: {err}"));
    assert_eq!(written.bytes(), b"\x1b[?25l");
}

#[test]
fn frame_backend_clear_writes_the_clear_sequence() {
    let written = CountingWriter::default();
    let mut backend = backend(written.clone());
    Backend::clear(&mut backend).unwrap_or_else(|err| panic!("clear: {err}"));
    assert!(
        written.bytes().is_empty(),
        "crossterm's flush stays buffered"
    );
    Backend::flush(&mut backend).unwrap_or_else(|err| panic!("flush: {err}"));
    assert_eq!(written.bytes(), b"\x1b[2J");
}

#[test]
fn frame_backend_returns_the_inner_cursor_and_size() {
    let written = CountingWriter::default();
    let mut backend = super::FrameBackend {
        inner: Calls::default(),
        writer: super::FrameWriter::new(written),
    };
    assert_eq!(
        Backend::get_cursor_position(&mut backend).unwrap_or_else(|err| panic!("cursor: {err}")),
        ratatui::layout::Position::new(3, 4)
    );
    assert_eq!(
        Backend::size(&backend).unwrap_or_else(|err| panic!("size: {err}")),
        ratatui::layout::Size::new(1, 1)
    );
    assert_eq!(backend.inner.0, ["get"]);
}

#[test]
fn frame_backend_flush_drains_buffered_bytes() {
    let written = CountingWriter::default();
    let mut backend = backend(written.clone());
    std::io::Write::write_all(&mut backend.inner, b"pending frame")
        .unwrap_or_else(|err| panic!("buffer frame: {err}"));
    assert!(written.bytes().is_empty(), "bytes wait for the frame flush");
    Backend::flush(&mut backend).unwrap_or_else(|err| panic!("flush: {err}"));
    assert_eq!(written.bytes(), b"pending frame");
}

/// One session line of `kind`, numbered `seq`.
fn envelope(kind: &str, action: Option<&str>, payload: serde_json::Value, seq: u64) -> Line {
    Line::Session(contract::Envelope {
        kind: EventKind::from_name(kind),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: Some(contract::Seq(seq)),
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

#[test]
fn a_wheel_frame_reaches_the_tty_in_one_write() {
    let written = CountingWriter::default();
    let mut screen = Screen::new(backend(written.clone()), 160, 48)
        .unwrap_or_else(|err| panic!("screen: {err}"));
    let mut app = session_app();
    screen
        .draw(&mut app, None)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    let bottom = app.scroll().0;
    app.on_wheel(&Mouse {
        kind: MouseKind::WheelUp,
        // Over the conversation, past the rail.
        col: 60,
        row: 10,
    });
    assert_eq!(app.top(), Some(bottom - 3));
    let calls = written.calls();
    screen
        .draw(&mut app, None)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    assert_eq!(written.calls() - calls, 1);
    assert!(written.bytes().ends_with(b"\x1b[?2026l"));
}
#[derive(Clone, Default)]
struct Bytes(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

/// Asserts two byte streams are equal without printing either whole: the
/// lengths first, then the first differing offset with a few bytes around it.
fn assert_same_bytes(got: &[u8], want: &[u8], what: &str) {
    assert_eq!(got.len(), want.len(), "{what}: byte lengths differ");
    if let Some(at) = got.iter().zip(want).position(|(a, b)| a != b) {
        let from = at.saturating_sub(8);
        let to = (at + 8).min(got.len());
        panic!(
            "{what}: first differing byte at {at}: got {}, want {}",
            got[from..to].escape_ascii(),
            want[from..to].escape_ascii()
        );
    }
}

impl std::io::Write for Bytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One painted frame: the word at the top-left corner.
fn render(
    _app: &App,
    _area: ratatui::layout::Rect,
    buf: &mut ratatui::buffer::Buffer,
    _pointer: Option<(u16, u16)>,
) -> Vec<Target> {
    buf.set_string(0, 0, "hi", ratatui::style::Style::default());
    Vec::new()
}

/// The synchronized-output markers in `bytes`, in order: true for a
/// begin, false for an end.
fn markers(bytes: &[u8]) -> Vec<bool> {
    const BEGIN: &[u8] = b"\x1b[?2026h";
    const END: &[u8] = b"\x1b[?2026l";
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at..].starts_with(BEGIN) {
            found.push(true);
            at += BEGIN.len();
        } else if bytes[at..].starts_with(END) {
            found.push(false);
            at += END.len();
        } else {
            at += 1;
        }
    }
    found
}

#[test]
fn every_frame_is_wrapped_in_synchronized_output() {
    let written = Bytes::default();
    let mut screen = Screen::new(CrosstermBackend::new(written.clone()), 40, 10)
        .unwrap_or_else(|err| panic!("screen: {err}"));
    let mut app = App::new(std::path::PathBuf::from("/w"));
    app.set_size(40, 10);
    let draw = |screen: &mut Screen<CrosstermBackend<Bytes>>, app: &mut App| {
        screen
            .draw_with(app, None, render)
            .unwrap_or_else(|err| panic!("draw: {err}"));
    };
    let bytes = || written.0.borrow().clone();
    // The first frame sits in one begin/end pair.
    draw(&mut screen, &mut app);
    assert_eq!(markers(&bytes()), [true, false]);
    // The redraw after a resize sits in one pair of its own: the pairs
    // never nest.
    screen
        .resize(30, 8)
        .unwrap_or_else(|err| panic!("resize: {err}"));
    app.set_size(30, 8);
    draw(&mut screen, &mut app);
    assert_eq!(markers(&bytes()), [true, false, true, false]);
    // An unchanged frame writes nothing: no cells, and no markers either.
    let before = bytes();
    draw(&mut screen, &mut app);
    assert_eq!(bytes(), before);
    // Dropping the screen closes the block: the cursor ratatui's own drop
    // would restore goes out inside one last pair, so no bare write is
    // left after the terminal is restored.
    drop(screen);
    assert_eq!(markers(&bytes()), [true, false, true, false, true, false]);
    assert!(
        bytes().ends_with(b"\x1b[?2026l"),
        "the stream ends with the block closed"
    );
}

/// An app showing 80 short turns at 160x48: deep enough to scroll.
fn session_app() -> App {
    let mut app = App::new(std::path::PathBuf::from("/w"));
    app.set_size(160, 48);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let mut seq = 0u64;
    let mut line = |kind: &str, action: Option<&str>, payload: serde_json::Value| {
        let line = envelope(kind, action, payload, seq);
        seq += 1;
        line
    };
    for turn in 0..80 {
        let message = format!("a_m{turn}");
        app.on_line(line(
            "turn_started",
            None,
            serde_json::json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("prompt {turn}")}]}]}),
        ));
        app.on_line(line(
            "text_completed",
            Some(&message),
            serde_json::json!({"text": "a one-line reply"}),
        ));
        app.on_line(line(
            "turn_completed",
            None,
            serde_json::json!({"outcome": "completed"}),
        ));
    }
    app
}

#[test]
fn buffering_writes_the_same_bytes_as_the_bare_backend() {
    let through = CountingWriter::default();
    let mut buffered = Screen::new(backend(through.clone()), 160, 48)
        .unwrap_or_else(|err| panic!("screen: {err}"));
    let plain = Bytes::default();
    let mut bare = Screen::new(CrosstermBackend::new(plain.clone()), 160, 48)
        .unwrap_or_else(|err| panic!("screen: {err}"));
    let mut app = session_app();
    let mut ref_app = session_app();
    let check = |what: &str, through: &CountingWriter, plain: &Bytes| {
        assert_same_bytes(&through.bytes(), &plain.0.borrow(), what);
    };
    buffered
        .draw(&mut app, None)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    bare.draw(&mut ref_app, None)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    check("draw", &through, &plain);
    buffered
        .resize(100, 30)
        .unwrap_or_else(|err| panic!("resize: {err}"));
    app.set_size(100, 30);
    bare.resize(100, 30)
        .unwrap_or_else(|err| panic!("resize: {err}"));
    ref_app.set_size(100, 30);
    check("resize", &through, &plain);
    buffered
        .draw(&mut app, None)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    bare.draw(&mut ref_app, None)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    check("redraw", &through, &plain);
    buffered
        .draw(&mut app, None)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    bare.draw(&mut ref_app, None)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    check("second redraw", &through, &plain);
}

#[test]
fn a_frame_bigger_than_the_buffer_arrives_whole() {
    // 600 by 300 cells draw about 80 KiB, past the 64 KiB reserve: the
    // buffer grows and the frame still arrives in one write.
    let written = CountingWriter::default();
    let mut screen = Screen::new(backend(written.clone()), 600, 300)
        .unwrap_or_else(|err| panic!("screen: {err}"));
    let mut app = session_app();
    app.set_size(600, 300);
    let calls = written.calls();
    screen
        .draw(&mut app, None)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    assert_eq!(written.calls() - calls, 1);
    let plain = Bytes::default();
    let mut bare = Screen::new(CrosstermBackend::new(plain.clone()), 600, 300)
        .unwrap_or_else(|err| panic!("screen: {err}"));
    let mut ref_app = session_app();
    ref_app.set_size(600, 300);
    bare.draw(&mut ref_app, None)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    assert_same_bytes(&written.bytes(), &plain.0.borrow(), "first frame");
    assert!(
        written.bytes().len() > super::FRAME_BUFFER,
        "first frame is {} bytes",
        written.bytes().len()
    );
}

/// A writer taking `limit` bytes, then failing every write: the prefix a
/// failed frame leaves on the wire.
#[derive(Clone, Default)]
struct FailingWriter {
    limit: std::rc::Rc<std::cell::Cell<usize>>,
    taken: std::rc::Rc<std::cell::Cell<usize>>,
    bytes: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
}

impl FailingWriter {
    fn bytes(&self) -> Vec<u8> {
        self.bytes.borrow().clone()
    }
}

impl std::io::Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let room = self.limit.get().saturating_sub(self.taken.get());
        let n = room.min(bytes.len());
        self.taken.set(self.taken.get().saturating_add(n));
        if let Some(prefix) = bytes.get(..n) {
            self.bytes.borrow_mut().extend_from_slice(prefix);
        }
        if n < bytes.len() {
            Err(std::io::Error::other("no room"))
        } else {
            Ok(n)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_failed_frame_leaves_nothing_behind() {
    let written = FailingWriter::default();
    written.limit.set(100);
    let mut screen = Screen::new(backend(written.clone()), 160, 48)
        .unwrap_or_else(|err| panic!("screen: {err}"));
    let mut app = session_app();
    if screen.draw(&mut app, None).is_ok() {
        panic!("a frame over the failing writer drew");
    }
    assert_eq!(written.bytes().len(), 100);
    written.limit.set(usize::MAX);
    screen
        .draw(&mut app, None)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    // The new frame starts with its begin marker: none of the failed
    // frame's bytes were written again.
    let bytes = written.bytes();
    let rest = bytes.get(100..).unwrap_or_default();
    assert!(
        rest.starts_with(b"\x1b[?2026h"),
        "second frame starts {rest:?}"
    );
    drop(screen);
    assert!(written.bytes().ends_with(b"\x1b[?2026l"));
}

/// Repaint clears the backend and invalidates the cached frame, so the
/// next draw rewrites whole.
#[test]
fn repaint_clears_the_backend_and_invalidates_the_frame() {
    let mut screen =
        Screen::new(TestBackend::new(40, 10), 40, 10).unwrap_or_else(|err| panic!("screen: {err}"));
    let mut app = App::new(std::path::PathBuf::from("/w"));
    app.set_size(40, 10);
    screen
        .draw_with(&mut app, None, render)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    assert!(screen.last().is_some(), "a painted screen caches its frame");
    assert_eq!(
        screen.backend().buffer()[(0, 0)].symbol(),
        "h",
        "the frame painted"
    );
    screen
        .repaint()
        .unwrap_or_else(|err| panic!("repaint: {err}"));
    assert!(screen.last().is_none(), "the cached frame is gone");
    assert_eq!(
        screen.backend().buffer()[(0, 0)].symbol(),
        " ",
        "the backend cleared"
    );
}

/// A backend recording the cells it receives in order: the draw-order
/// tests assert the cell order (`docs/tui.md`, "Wide variation-selector
/// cells"), not which call carried each cell.
#[derive(Default)]
struct Order {
    cells: Vec<(u16, u16, String)>,
}

impl Order {
    fn ordered_cells(&self) -> &[(u16, u16, String)] {
        &self.cells
    }
}

impl SyncEmit for Order {
    fn emit(&mut self, _begin: bool) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Backend for Order {
    type Error = std::io::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.cells
            .extend(content.map(|(x, y, cell)| (x, y, cell.symbol().to_owned())));
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        Ok(ratatui::layout::Position::new(3, 4))
    }

    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        _position: P,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        let _ = clear_type;
        Ok(())
    }

    fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
        Ok(ratatui::layout::Size::new(1, 1))
    }

    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
        Ok(ratatui::backend::WindowSize {
            columns_rows: ratatui::layout::Size::new(1, 1),
            pixels: ratatui::layout::Size::new(1, 1),
        })
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// A `TtySized` over the recording backend: the draw-order tests' backend.
fn ordered() -> super::TtySized<Order> {
    super::TtySized {
        inner: Order::default(),
        size: ratatui::layout::Size::new(40, 10),
        open: false,
        emit: Order::emit,
        touched: false,
    }
}

/// A cell holding `symbol`.
fn held(symbol: &str) -> ratatui::buffer::Cell {
    let mut cell = ratatui::buffer::Cell::default();
    cell.set_symbol(symbol);
    cell
}

/// The emoji, its trailing cell and the next cell reach the inner backend
/// trailing first: on a terminal giving the emoji two cells, the trailing
/// blank would otherwise print one column right.
#[test]
fn a_variation_selector_emoji_trades_places_with_its_trailing_cell() {
    let emoji = held("⚠️");
    let trailing = held(" ");
    let after = held("c");
    let mut backend = ordered();
    Backend::draw(
        &mut backend,
        [(3, 0, &emoji), (4, 0, &trailing), (5, 0, &after)].into_iter(),
    )
    .unwrap();
    assert_eq!(
        // (`docs/tui.md`, "Wide variation-selector cells")
        backend.inner.ordered_cells(),
        [
            (4, 0, " ".to_owned()),
            (3, 0, "⚠️".to_owned()),
            (5, 0, "c".to_owned()),
        ]
    );
}

/// The emoji with no adjacent trailing cell keeps its place: the next
/// cell at a gap stays after it, so a later trailing yield cannot join it.
#[test]
fn a_variation_selector_emoji_without_a_trailing_cell_keeps_order() {
    let emoji = held("⚠️");
    let after = held("c");
    let mut backend = ordered();
    Backend::draw(&mut backend, [(3, 0, &emoji), (5, 0, &after)].into_iter()).unwrap();
    assert_eq!(
        // (`docs/tui.md`, "Wide variation-selector cells")
        backend.inner.ordered_cells(),
        [(3, 0, "⚠️".to_owned()), (5, 0, "c".to_owned()),]
    );
}

/// Two emoji in a row each trade places with their own trailing cell: the
/// second pair is untouched by the first one's swap.
#[test]
fn two_variation_selector_emojis_each_trade_places() {
    let first = held("⚠️");
    let first_trailing = held(" ");
    let second = held("❤️");
    let second_trailing = held(" ");
    let after = held("d");
    let mut backend = ordered();
    Backend::draw(
        &mut backend,
        [
            (3, 0, &first),
            (4, 0, &first_trailing),
            (5, 0, &second),
            (6, 0, &second_trailing),
            (7, 0, &after),
        ]
        .into_iter(),
    )
    .unwrap();
    assert_eq!(
        // (`docs/tui.md`, "Wide variation-selector cells")
        backend.inner.ordered_cells(),
        [
            (4, 0, " ".to_owned()),
            (3, 0, "⚠️".to_owned()),
            (6, 0, " ".to_owned()),
            (5, 0, "❤️".to_owned()),
            (7, 0, "d".to_owned()),
        ]
    );
}

/// A wide cell with no variation selector keeps its order even when the
/// next cell is adjacent: only the selector pair reorders, never every
/// wide cell.
#[test]
fn a_wide_cell_without_a_variation_selector_keeps_order() {
    let wide = held("界");
    let after = held("c");
    let mut backend = ordered();
    Backend::draw(&mut backend, [(3, 0, &wide), (4, 0, &after)].into_iter()).unwrap();
    assert_eq!(
        // (`docs/tui.md`, "Wide variation-selector cells")
        backend.inner.ordered_cells(),
        [(3, 0, "界".to_owned()), (4, 0, "c".to_owned())]
    );
}

/// A cell one column wide that still holds a variation selector keeps its
/// order: the width check needs more than one column, not at least one.
#[test]
fn a_narrow_cell_holding_a_variation_selector_keeps_order() {
    use ratatui::buffer::CellWidth as _;
    let narrow = held("a\u{FE0F}");
    assert_eq!(
        narrow.symbol().cell_width(),
        1,
        "the narrow variation-selector cell measures one column"
    );
    let after = held("c");
    let mut backend = ordered();
    Backend::draw(&mut backend, [(3, 0, &narrow), (4, 0, &after)].into_iter()).unwrap();
    assert_eq!(
        // (`docs/tui.md`, "Wide variation-selector cells")
        backend.inner.ordered_cells(),
        [(3, 0, "a\u{FE0F}".to_owned()), (4, 0, "c".to_owned()),]
    );
}

/// A trailing emoji with nothing after it draws alone, with no panic on
/// the missing next cell.
#[test]
fn a_trailing_variation_selector_emoji_draws_alone() {
    let emoji = held("⚠️");
    let mut backend = ordered();
    Backend::draw(&mut backend, [(3, 0, &emoji)].into_iter()).unwrap();
    // (`docs/tui.md`, "Wide variation-selector cells")
    assert_eq!(backend.inner.ordered_cells(), [(3, 0, "⚠️".to_owned())]);
}

/// The emoji does not pair with the next row's first cell: only the cell
/// at the next column of the same row is its trailing cell.
#[test]
fn a_variation_selector_emoji_does_not_pair_with_the_next_rows_first_cell() {
    let emoji = held("⚠️");
    let next_row = held("a");
    let mut backend = ordered();
    Backend::draw(
        &mut backend,
        [(3, 0, &emoji), (0, 1, &next_row)].into_iter(),
    )
    .unwrap();
    assert_eq!(
        // (`docs/tui.md`, "Wide variation-selector cells")
        backend.inner.ordered_cells(),
        [(3, 0, "⚠️".to_owned()), (0, 1, "a".to_owned()),]
    );
}

/// The emoji does not pair with an earlier column of its row: the swap
/// needs the next column exactly, not at most it.
#[test]
fn a_variation_selector_emoji_does_not_pair_with_an_earlier_column() {
    let emoji = held("⚠️");
    let earlier = held("a");
    let mut backend = ordered();
    Backend::draw(&mut backend, [(3, 0, &emoji), (2, 0, &earlier)].into_iter()).unwrap();
    assert_eq!(
        // (`docs/tui.md`, "Wide variation-selector cells")
        backend.inner.ordered_cells(),
        [(3, 0, "⚠️".to_owned()), (2, 0, "a".to_owned()),]
    );
}

/// The emoji does not pair with the cell under its trailing column on the
/// next row: the swap needs the same row, not just the next column.
#[test]
fn a_variation_selector_emoji_does_not_pair_with_the_cell_below_its_trailing_column() {
    let emoji = held("⚠️");
    let below = held("b");
    let mut backend = ordered();
    Backend::draw(&mut backend, [(3, 0, &emoji), (4, 1, &below)].into_iter()).unwrap();
    assert_eq!(
        // (`docs/tui.md`, "Wide variation-selector cells")
        backend.inner.ordered_cells(),
        [(3, 0, "⚠️".to_owned()), (4, 1, "b".to_owned()),]
    );
}

/// Empty content still opens the frame's synchronized-output block, as
/// before: no cursor-only frame turns into missing bytes (`docs/tui.md`,
/// "Performance").
#[test]
fn empty_content_still_reaches_the_inner_backend_once() {
    let mut backend = ordered();
    let empty: [(u16, u16, &ratatui::buffer::Cell); 0] = [];
    Backend::draw(&mut backend, empty.into_iter()).unwrap();
    assert!(backend.open, "the empty frame opened its block");
    assert!(backend.inner.ordered_cells().is_empty());
}

/// Take_touched consumes the mark: true once after a frame wrote,
/// false right after, with no wait.
#[test]
fn take_touched_consumes_the_mark() {
    let mut screen =
        Screen::new(TestBackend::new(40, 10), 40, 10).unwrap_or_else(|err| panic!("screen: {err}"));
    let mut app = App::new(std::path::PathBuf::from("/w"));
    app.set_size(40, 10);
    assert!(!screen.take_touched(), "nothing wrote yet");
    screen
        .draw_with(&mut app, None, render)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    assert!(screen.take_touched(), "the frame wrote");
    assert!(!screen.take_touched(), "the mark is consumed");
}

// The frame-shape ring: painted shapes with the scroll state that
// produced each one (`docs/tui.md`, "Slash commands").

// The look for `setting` on a terminal with `vars` set.
fn look(setting: ThemeSetting, vars: &[(&str, &str)]) -> Look {
    let vars: Vec<(String, String)> = vars
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    let (look, notice) = Look::new(setting, &|name| {
        vars.iter()
            .find(|(set, _)| set == name)
            .map(|(_, value)| value.clone())
    });
    assert_eq!(notice, None);
    look
}

/// A screen at `width` by `height` with no frames captured.
fn ring_screen(width: u16, height: u16) -> Screen<TestBackend> {
    Screen::new(TestBackend::new(width, height), width, height)
        .unwrap_or_else(|err| panic!("screen: {err}"))
}

/// An app with no session: following, with no new rows waiting.
fn plain_app(width: u16, height: u16) -> App {
    let mut app = App::new(std::path::PathBuf::from("/w"));
    app.set_size(width, height);
    app
}

/// Draws `paint` over `app` on `screen`: one captured frame per call.
#[track_caller]
fn draw(
    screen: &mut Screen<TestBackend>,
    app: &mut App,
    paint: impl FnMut(
        &App,
        ratatui::layout::Rect,
        &mut ratatui::buffer::Buffer,
        Option<(u16, u16)>,
    ) -> Vec<Target>,
) {
    screen
        .draw_with(app, None, paint)
        .unwrap_or_else(|err| panic!("draw: {err}"));
}

/// The shape the ring's newest frame must hold: the kept frame's
/// buffer painted with the screen's look, shaped with the app's scroll
/// state. Every ring test below ends on this recomputation.
#[track_caller]
fn painted_shape(screen: &Screen<TestBackend>, app: &App) -> FrameShape {
    let (buffer, _) = screen.last().expect("a frame was drawn");
    let mut painted = buffer.clone();
    screen.look.paint(&mut painted);
    super::shape_frame(&painted, app.top(), app.has_new())
}

/// The ring's newest frame equals its recomputed painted shape.
#[track_caller]
fn assert_round_trips(screen: &Screen<TestBackend>, app: &App) {
    let shapes = screen.frame_shapes();
    let newest = shapes.last().expect("the ring holds a frame");
    assert_eq!(newest, &painted_shape(screen, app));
}

/// The foreground of the cell at (`col`, `row`) of the ring's newest
/// frame.
fn shaped_fg(screen: &Screen<TestBackend>, col: usize, row: usize) -> StyleValue {
    let shapes = screen.frame_shapes();
    let frame = shapes.last().expect("the ring holds a frame");
    let cell = frame.rows[row].cells()[col];
    frame.palette[cell.style()]
}

/// The ring starts empty: no frame drew yet.
#[test]
fn the_ring_starts_empty() {
    let screen = ring_screen(80, 24);
    assert!(screen.frame_shapes().is_empty());
}

/// The ring keeps the newest frames up to its depth, then drops the
/// oldest exactly one per frame: 19 draws keep 19, 20 keep 20, and the
/// 21st drops the first draw.
// `docs/user/reporting.md`, "Diagnostics" (at most the newest 20 frame shapes) plus
// docs/tui.md `/feedback` row (last 20 painted frames, always on).
#[test]
fn the_ring_keeps_the_newest_twenty_frames() {
    let mut screen = ring_screen(8, 1);
    screen.set_look(look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]));
    let mut app = plain_app(8, 1);
    // `docs/user/reporting.md`, "Diagnostics" plus docs/tui.md `/feedback` row: the ring depth is 20.
    assert_eq!(RING_DEPTH, 20);
    for draw_at in 0..19 {
        let marker = draw_at;
        draw(&mut screen, &mut app, |_, _, buf, _| {
            buf.set_string(0, 0, "x", ratatui::style::Style::new());
            if let Some(cell) = buf.cell_mut((0, 0)) {
                cell.set_fg(ratatui::style::Color::Rgb(marker, 0, 0));
            }
            Vec::new()
        });
        assert_round_trips(&screen, &app);
    }
    assert_eq!(screen.frame_shapes().len(), 19);
    draw(&mut screen, &mut app, |_, _, buf, _| {
        buf.set_string(0, 0, "x", ratatui::style::Style::new());
        if let Some(cell) = buf.cell_mut((0, 0)) {
            cell.set_fg(ratatui::style::Color::Rgb(19, 0, 0));
        }
        Vec::new()
    });
    assert_eq!(screen.frame_shapes().len(), 20);
    assert_eq!(
        shaped_fg(&screen, 0, 0).fg,
        ShapeColor::Rgb(19, 0, 0),
        "20 draws keep all 20"
    );
    draw(&mut screen, &mut app, |_, _, buf, _| {
        buf.set_string(0, 0, "x", ratatui::style::Style::new());
        if let Some(cell) = buf.cell_mut((0, 0)) {
            cell.set_fg(ratatui::style::Color::Rgb(20, 0, 0));
        }
        Vec::new()
    });
    let shapes = screen.frame_shapes();
    assert_eq!(shapes.len(), 20);
    let oldest = shapes[0].rows[0].cells()[0];
    assert_eq!(
        shapes[0].palette[oldest.style()].fg,
        ShapeColor::Rgb(1, 0, 0),
        "the 21st draw drops exactly the first"
    );
    let frame = shapes.last().expect("the ring holds a frame");
    let cell = frame.rows[0].cells()[0];
    assert_eq!(frame.palette[cell.style()].fg, ShapeColor::Rgb(20, 0, 0));
    assert_round_trips(&screen, &app);
}

/// Each shaped row covers exactly its width: the walk stops at the row's
/// end, so `x < width` never reads the next row's first cell. With `<=` the
/// first of two rows would hold one extra cell.
// `docs/user/reporting.md`, "Diagnostics" (per cell plus the frame geometry): rows stay
// width-wide, so the report can place each cell.
#[test]
fn shaped_rows_cover_exactly_their_width() {
    let mut screen = ring_screen(8, 2);
    let mut app = plain_app(8, 2);
    draw(&mut screen, &mut app, |_, _, _, _| Vec::new());
    let shapes = screen.frame_shapes();
    let frame = shapes.last().expect("the ring holds a frame");
    assert_eq!(frame.rows.len(), 2);
    for row in &frame.rows {
        assert_eq!(row.cells().len(), 8, "every row covers exactly its width");
    }
    assert_round_trips(&screen, &app);
}

/// A frame equal to the last writes nothing and records nothing.
#[test]
fn equal_frames_record_nothing() {
    let mut screen = ring_screen(8, 1);
    let mut app = plain_app(8, 1);
    draw(&mut screen, &mut app, |_, _, buf, _| {
        buf.set_string(0, 0, "same", ratatui::style::Style::new());
        Vec::new()
    });
    assert_eq!(screen.frame_shapes().len(), 1);
    draw(&mut screen, &mut app, |_, _, buf, _| {
        buf.set_string(0, 0, "same", ratatui::style::Style::new());
        Vec::new()
    });
    assert_eq!(
        screen.frame_shapes().len(),
        1,
        "the equal frame recorded nothing"
    );
}

/// A red cell and a blue cell land on distinct palette entries: two
/// distinct colours never encode identically.
#[test]
fn red_and_blue_cells_land_on_distinct_palette_entries() {
    let mut screen = ring_screen(8, 1);
    screen.set_look(look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]));
    let mut app = plain_app(8, 1);
    draw(&mut screen, &mut app, |_, _, buf, _| {
        buf.set_string(0, 0, "rb", ratatui::style::Style::new());
        if let Some(cell) = buf.cell_mut((0, 0)) {
            cell.set_fg(ratatui::style::Color::Red);
        }
        if let Some(cell) = buf.cell_mut((1, 0)) {
            cell.set_fg(ratatui::style::Color::Blue);
        }
        Vec::new()
    });
    let red = shaped_fg(&screen, 0, 0);
    let blue = shaped_fg(&screen, 1, 0);
    assert_eq!(red.fg, ShapeColor::Red);
    assert_eq!(blue.fg, ShapeColor::Blue);
    assert_ne!(red, blue);
    assert_round_trips(&screen, &app);
}

/// The diff options classify distinctly: `Skip` shapes exempt,
/// `AlwaysUpdate` classifies normally, a forced width of 3 shapes a
/// lead plus two continuations, and an emptied symbol shapes narrow
/// blank. The covered cells under the forced width never surface. The
/// deprecated `skip` flag exempts exactly like `Skip`.
#[allow(
    deprecated,
    reason = "pins the legacy `skip` flag the capture still reads"
)]
#[test]
fn diff_options_classify_distinctly() {
    use ratatui::buffer::CellDiffOption;
    let mut screen = ring_screen(8, 1);
    screen.set_look(look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]));
    let mut app = plain_app(8, 1);
    draw(&mut screen, &mut app, |_, _, buf, _| {
        buf.set_string(0, 0, "abcdef", ratatui::style::Style::new());
        if let Some(cell) = buf.cell_mut((0, 0)) {
            cell.set_diff_option(CellDiffOption::Skip);
        }
        if let Some(cell) = buf.cell_mut((1, 0)) {
            cell.set_diff_option(CellDiffOption::AlwaysUpdate);
        }
        if let Some(cell) = buf.cell_mut((2, 0)) {
            cell.set_diff_option(CellDiffOption::ForcedWidth(
                std::num::NonZeroU16::new(3).unwrap(),
            ));
        }
        // Covered by the forced width: never read, so never shaped.
        if let Some(cell) = buf.cell_mut((3, 0)) {
            cell.set_fg(ratatui::style::Color::Red);
            cell.set_diff_option(CellDiffOption::Skip);
        }
        if let Some(cell) = buf.cell_mut((4, 0)) {
            cell.set_fg(ratatui::style::Color::Blue);
            cell.set_diff_option(CellDiffOption::Skip);
        }
        if let Some(cell) = buf.cell_mut((5, 0)) {
            cell.set_symbol("");
        }
        if let Some(cell) = buf.cell_mut((6, 0)) {
            cell.set_skip(true);
        }
        Vec::new()
    });
    let shapes = screen.frame_shapes();
    let frame = shapes.last().expect("the ring holds a frame");
    let row = &frame.rows[0];
    assert_eq!(row.cells().len(), 8, "every row covers exactly its width");
    assert_eq!(row.cells()[0].kind(), ShapeKind::Exempt);
    assert!(!row.cells()[0].wide());
    assert_eq!(row.cells()[1].kind(), ShapeKind::Content);
    assert!(!row.cells()[1].wide());
    assert_eq!(row.cells()[2].kind(), ShapeKind::Content);
    assert!(row.cells()[2].wide(), "the forced width leads wide");
    assert_eq!(row.cells()[3].kind(), ShapeKind::Continuation);
    assert_eq!(row.cells()[4].kind(), ShapeKind::Continuation);
    assert_eq!(row.cells()[5].kind(), ShapeKind::Blank);
    assert!(!row.cells()[5].wide(), "the emptied symbol is narrow");
    assert_eq!(
        row.cells()[6].kind(),
        ShapeKind::Exempt,
        "the legacy flag exempts like `Skip`"
    );
    for cell in row.cells() {
        let style = frame.palette[cell.style()];
        assert!(
            !matches!(style.fg, ShapeColor::Red | ShapeColor::Blue),
            "covered cells never surface: {style:?}"
        );
    }
    assert_round_trips(&screen, &app);
}

/// A role-marker cell shapes as its painted colour with DIM, never the
/// marker: the muted role under a fixed light look.
#[test]
fn a_role_marker_shapes_as_its_painted_colour_with_dim() {
    let mut screen = ring_screen(8, 1);
    screen.set_look(look(ThemeSetting::Light, &[("COLORTERM", "truecolor")]));
    let mut app = plain_app(8, 1);
    draw(&mut screen, &mut app, |_, _, buf, _| {
        buf.set_string(0, 0, "m", ratatui::style::Style::new());
        if let Some(cell) = buf.cell_mut((0, 0)) {
            cell.set_fg(Role::Muted.color());
        }
        Vec::new()
    });
    assert!(screen.look.dim(Role::Muted), "muted draws dim");
    let shaped = shaped_fg(&screen, 0, 0);
    let painted = screen.look.colour(Role::Muted);
    let expected = match painted {
        ratatui::style::Color::Reset => ShapeColor::Reset,
        ratatui::style::Color::Black => ShapeColor::Black,
        ratatui::style::Color::Red => ShapeColor::Red,
        ratatui::style::Color::Green => ShapeColor::Green,
        ratatui::style::Color::Yellow => ShapeColor::Yellow,
        ratatui::style::Color::Blue => ShapeColor::Blue,
        ratatui::style::Color::Magenta => ShapeColor::Magenta,
        ratatui::style::Color::Cyan => ShapeColor::Cyan,
        ratatui::style::Color::Gray => ShapeColor::Gray,
        ratatui::style::Color::DarkGray => ShapeColor::DarkGray,
        ratatui::style::Color::LightRed => ShapeColor::LightRed,
        ratatui::style::Color::LightGreen => ShapeColor::LightGreen,
        ratatui::style::Color::LightYellow => ShapeColor::LightYellow,
        ratatui::style::Color::LightBlue => ShapeColor::LightBlue,
        ratatui::style::Color::LightMagenta => ShapeColor::LightMagenta,
        ratatui::style::Color::LightCyan => ShapeColor::LightCyan,
        ratatui::style::Color::White => ShapeColor::White,
        ratatui::style::Color::Indexed(index) => ShapeColor::Indexed(index),
        ratatui::style::Color::Rgb(red, green, blue) => ShapeColor::Rgb(red, green, blue),
    };
    assert_eq!(shaped.fg, expected);
    assert_ne!(
        shaped.fg,
        ShapeColor::Indexed(Role::Muted as u8),
        "the marker resolved, it never reaches the ring raw"
    );
    assert_ne!(
        shaped.mods & ratatui::style::Modifier::DIM.bits(),
        0,
        "the dim role gains DIM"
    );
    assert_round_trips(&screen, &app);
}

/// A tinted half-block edge shapes blank with no colour and content in
/// truecolour: the same render under both looks.
#[test]
fn no_color_blanks_half_block_edges() {
    let edge = |_: &App,
                _: ratatui::layout::Rect,
                buf: &mut ratatui::buffer::Buffer,
                _: Option<(u16, u16)>| {
        buf.set_string(0, 0, "x", ratatui::style::Style::new());
        if let Some(cell) = buf.cell_mut((0, 0)) {
            cell.set_symbol("▄");
            cell.set_fg(Role::Panel.color());
        }
        Vec::new()
    };
    assert!(Role::Panel.tint(), "the panel role is a tint");
    let mut plain = ring_screen(8, 1);
    plain.set_look(look(ThemeSetting::Dark, &[("NO_COLOR", "1")]));
    let mut app = plain_app(8, 1);
    draw(&mut plain, &mut app, edge);
    let shapes = plain.frame_shapes();
    let frame = shapes.last().expect("the ring holds a frame");
    assert_eq!(frame.rows[0].cells()[0].kind(), ShapeKind::Blank);
    let mut vivid = ring_screen(8, 1);
    vivid.set_look(look(ThemeSetting::Dark, &[("COLORTERM", "truecolor")]));
    draw(&mut vivid, &mut app, edge);
    let shapes = vivid.frame_shapes();
    let frame = shapes.last().expect("the ring holds a frame");
    assert_eq!(frame.rows[0].cells()[0].kind(), ShapeKind::Content);
    assert_round_trips(&plain, &app);
    assert_round_trips(&vivid, &app);
}

/// A session's stream: durable lines numbered from 0, one turn per
/// message pair.
struct Stream {
    lines: Vec<contract::Envelope>,
    seq: u64,
    ts: u64,
}

impl Stream {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            seq: 0,
            ts: 1_000,
        }
    }

    fn line(&mut self, kind: &str, action: Option<&str>, payload: serde_json::Value) {
        self.ts += 700;
        self.seq += 1;
        self.lines.push(contract::Envelope {
            kind: EventKind::from_name(kind),
            session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
            ts: self.ts,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: action.map(|id| contract::ActionId(id.to_owned())),
            seq: Some(contract::Seq(self.seq - 1)),
            payload: payload.as_object().cloned().unwrap_or_default(),
        });
    }

    /// One turn: a prompt and one wrapped reply of several rows.
    fn turn(&mut self, turn: usize) {
        let message = format!("m{turn}");
        self.line(
            "turn_started",
            None,
            serde_json::json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("prompt {turn}: look at the code")}]}]}),
        );
        self.line("step_started", None, serde_json::json!({}));
        self.line(
            "assistant_message_started",
            Some(&message),
            serde_json::json!({}),
        );
        let words = "the quick brown fox jumps over the lazy dog ".repeat(3);
        self.line(
            "text_completed",
            Some(&message),
            serde_json::json!({"text": format!("reply {turn}: {words}")}),
        );
        self.line(
            "assistant_message_completed",
            Some(&message),
            serde_json::json!({"outcome": "completed"}),
        );
        self.line(
            "turn_completed",
            None,
            serde_json::json!({"outcome": "completed"}),
        );
    }
}

/// The durable lines in `range`, as `history` answers.
fn history(
    lines: &[contract::Envelope],
    range: &std::ops::RangeInclusive<contract::Seq>,
) -> Vec<contract::Envelope> {
    lines
        .iter()
        .filter(|line| line.seq.is_some_and(|seq| range.contains(&seq)))
        .cloned()
        .collect()
}

/// Loads every page the app needs from `lines`.
fn frame(app: &mut App, lines: &[contract::Envelope]) {
    while let Some((session, range)) = app.needs().first().cloned() {
        app.load(&session, history(lines, &range));
        assert_ne!(
            app.needs().first(),
            Some(&(session, range)),
            "loading did not settle"
        );
    }
}

/// An app attached to `lines` at 80 by 24, every page loaded.
fn scrolled_app(lines: &[contract::Envelope]) -> App {
    let mut app = App::new(std::path::PathBuf::from("/w"));
    app.on_line(Line::Hub(contract::HubLine {
        kind: contract::HubEventKind::HubHello,
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }));
    app.set_size(80, 24);
    app.attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    open(&mut app, lines);
    app
}

/// Folds `lines` into `app`, a frame a line.
fn open(app: &mut App, lines: &[contract::Envelope]) {
    for line in lines {
        app.on_line(Line::Session(line.clone()));
        frame(app, lines);
    }
}

/// A wheel step over the conversation: up scrolls up three rows, down
/// scrolls down three.
fn wheel(up: bool) -> Mouse {
    Mouse {
        kind: if up {
            MouseKind::WheelUp
        } else {
            MouseKind::WheelDown
        },
        col: 10,
        row: 5,
    }
}

/// A known scroll sequence produces the expected shape frames: follow,
/// up three, new output while scrolled, then down back to follow. Each
/// frame carries the geometry and the scroll state that produced it.
#[test]
fn scroll_sequence_shapes_carry_the_scroll_state() {
    let mut stream = Stream::new();
    for turn in 0..30 {
        stream.turn(turn);
    }
    let mut app = scrolled_app(&stream.lines);
    assert!(
        app.scroll().1 > 24,
        "the session overflows the viewport: {:?}",
        app.scroll()
    );
    let mut screen = ring_screen(80, 24);
    let mut want: Vec<(Option<usize>, bool)> = Vec::new();
    let mut recomputed: Vec<FrameShape> = Vec::new();
    // Following: no top, nothing new.
    assert_eq!((app.top(), app.has_new()), (None, false));
    want.push((app.top(), app.has_new()));
    draw(&mut screen, &mut app, crate::view::render);
    recomputed.push(painted_shape(&screen, &app));
    // Up three: the top pins a scrolled row.
    app.on_wheel(&wheel(true));
    assert!(app.top().is_some(), "the wheel scrolled up");
    assert!(!app.has_new());
    want.push((app.top(), app.has_new()));
    draw(&mut screen, &mut app, crate::view::render);
    recomputed.push(painted_shape(&screen, &app));
    // New output while scrolled: the frame notes rows wait below.
    let more = stream.lines.len();
    stream.turn(30);
    open(&mut app, &stream.lines[more..]);
    assert!(app.top().is_some(), "the new output keeps the scroll");
    assert!(app.has_new(), "new output while scrolled waits below");
    want.push((app.top(), app.has_new()));
    draw(&mut screen, &mut app, crate::view::render);
    recomputed.push(painted_shape(&screen, &app));
    // Down to the tail: following again.
    for _ in 0..40 {
        if app.top().is_none() {
            break;
        }
        app.on_wheel(&wheel(false));
    }
    assert_eq!((app.top(), app.has_new()), (None, false));
    want.push((app.top(), app.has_new()));
    draw(&mut screen, &mut app, crate::view::render);
    recomputed.push(painted_shape(&screen, &app));
    let shapes = screen.frame_shapes();
    assert_eq!(shapes.len(), want.len());
    for (frame, (top, has_new)) in shapes.iter().zip(&want) {
        assert_eq!(frame.width, 80);
        assert_eq!(frame.height, 24);
        assert_eq!(&frame.top, top);
        assert_eq!(&frame.has_new, has_new);
    }
    assert_eq!(want[0], (None, false));
    assert!(want[1].0.is_some());
    assert!(want[2].0.is_some() && want[2].1);
    assert_eq!(want[3], (None, false));
    // The dumped ring equals the recomputed painted shapes.
    assert_eq!(shapes, recomputed);
}
