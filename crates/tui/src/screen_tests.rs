//! Tests for the screen: its viewport size and the backend that reports it.

use super::{Screen, SyncEmit, backend};
use crate::app::App;
use crate::keys::{Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::Target;
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
        kind: kind.to_owned(),
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
        col: 20,
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
