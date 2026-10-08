//! The screen: ratatui on a fixed viewport sized from the injected tty, the
//! last frame drawn and its click targets (`docs/tui.md`, "Mouse and
//! hover"). A frame equal to the last writes nothing.

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Rect, Size};
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::app::App;
use crate::mouse::Target;
use crate::view;

/// One frame: the cells, and where the cursor shows, if anywhere.
type Frame = (Buffer, Option<Position>);

/// The screen: ratatui on a fixed viewport, and the last frame drawn with
/// its click targets.
pub(crate) struct Screen<B: Backend> {
    terminal: Terminal<TtySized<B>>,
    area: Rect,
    last: Option<Frame>,
    /// The click targets of the last frame drawn: what a click hits.
    targets: Vec<Target>,
}

impl<B: Backend> Screen<B> {
    pub(crate) fn new(backend: B, width: u16, height: u16) -> Result<Self, B::Error> {
        let area = Rect::new(0, 0, width, height);
        let terminal = Terminal::with_options(
            TtySized {
                inner: backend,
                size: area.as_size(),
            },
            TerminalOptions {
                viewport: Viewport::Fixed(area),
            },
        )?;
        Ok(Self {
            terminal,
            area,
            last: None,
            targets: Vec::new(),
        })
    }

    /// Draws `app`, the cursor shown at the draft's cursor or hidden,
    /// tinting the click target under `pointer`, and keeps the frame's
    /// targets. When the frame drops the focused target, focus returns to
    /// the input box and the frame is drawn again, so the cursor shows.
    /// A frame whose cells and cursor equal the last one's writes
    /// nothing; otherwise only the cells that changed are written.
    pub(crate) fn draw(
        &mut self,
        app: &mut App,
        pointer: Option<(u16, u16)>,
    ) -> Result<(), B::Error> {
        self.draw_with(app, pointer, view::render)
    }

    pub(crate) fn draw_with(
        &mut self,
        app: &mut App,
        pointer: Option<(u16, u16)>,
        mut render: impl FnMut(&App, Rect, &mut Buffer, Option<(u16, u16)>) -> Vec<Target>,
    ) -> Result<(), B::Error> {
        let mut cells = Buffer::empty(self.area);
        self.targets = render(app, self.area, &mut cells, pointer);
        if app.drawn(&self.targets) {
            cells = Buffer::empty(self.area);
            self.targets = render(app, self.area, &mut cells, pointer);
            let _ = app.drawn(&self.targets);
        }
        let next = (cells, view::cursor(app, self.area));
        if self.last.as_ref() == Some(&next) {
            return Ok(());
        }
        self.terminal.draw(|frame| {
            frame.buffer_mut().clone_from(&next.0);
            if let Some(cursor) = next.1 {
                frame.set_cursor_position(cursor);
            }
        })?;
        self.last = Some(next);
        Ok(())
    }

    /// Resizes the viewport; the next draw repaints it whole.
    pub(crate) fn resize(&mut self, width: u16, height: u16) -> Result<(), B::Error> {
        self.area = Rect::new(0, 0, width, height);
        self.last = None;
        self.terminal.backend_mut().size = self.area.as_size();
        self.terminal.resize(self.area)
    }

    /// The click targets of the last frame drawn: what a click hits.
    pub(crate) fn targets(&self) -> &[Target] {
        &self.targets
    }

    /// The viewport's size.
    pub(crate) fn area(&self) -> Rect {
        self.area
    }

    /// The backend under the size the tty reported.
    #[cfg(test)]
    pub(crate) fn backend(&self) -> &B {
        &self.terminal.backend().inner
    }

    /// The backend under the size the tty reported, to change.
    #[cfg(test)]
    pub(crate) fn backend_mut(&mut self) -> &mut B {
        &mut self.terminal.backend_mut().inner
    }

    /// The last frame drawn.
    #[cfg(test)]
    pub(crate) fn last(&self) -> Option<&(Buffer, Option<Position>)> {
        self.last.as_ref()
    }
}

/// A backend that reports the size read from the injected tty. ratatui
/// asks its backend for the size when it clears a fixed viewport on
/// resize, and crossterm answers from `/dev/tty`, standard output or
/// `tput`, never from the injected tty: with none of those, as under a
/// test harness, the answer is an error and the resize fails.
struct TtySized<B> {
    inner: B,
    size: Size,
}

impl<B: Backend> Backend for TtySized<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        Ok(self.size)
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        Ok(WindowSize {
            columns_rows: self.size,
            pixels: Size::default(),
        })
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
