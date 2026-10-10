//! Tests for the screen: its viewport size and the backend that reports it.

use super::{Screen, SyncEmit};
use crate::app::App;
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
    type Error = std::convert::Infallible;

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
    };
    let cell = ratatui::buffer::Cell::default();
    let ok = |result: Result<(), std::convert::Infallible>| result.unwrap_or(());
    ok(backend.draw([(0, 0, &cell), (1, 0, &cell)].into_iter()));
    ok(backend.hide_cursor());
    ok(backend.show_cursor());
    let position = backend
        .get_cursor_position()
        .unwrap_or_else(|err| match err {});
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
    let size = backend.size().unwrap_or_else(|err| match err {});
    assert_eq!(size, ratatui::layout::Size::new(40, 10));
}

/// A byte sink behind `CrosstermBackend` that keeps every byte written.
#[derive(Clone, Default)]
struct Bytes(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

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
}
