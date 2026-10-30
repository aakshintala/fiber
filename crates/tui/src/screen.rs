//! The screen: ratatui on a fixed viewport sized from the injected tty, the
//! last frame drawn and its click targets (`docs/tui.md`, "Mouse and
//! hover"). A frame equal to the last writes nothing. Every frame is
//! written inside synchronized output, mode 2026 (`docs/tui.md`,
//! "Performance").
//!
//! Frames are drawn with role markers (`crate::theme`); the frame kept for
//! the "nothing changed" comparison keeps them, and only the copy written
//! to the terminal is painted with the look's colours.

use ratatui::backend::{Backend, ClearType, CrosstermBackend, TestBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::Style;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::app::App;
use crate::look::{Appearance, Look};
use crate::mouse::Target;
use crate::theme::Role;
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
    /// The theme and colour depth frames are painted with.
    look: Look,
}

impl<B: Backend> Screen<B> {
    pub(crate) fn new(backend: B, width: u16, height: u16) -> Result<Self, B::Error>
    where
        B: SyncEmit,
    {
        let area = Rect::new(0, 0, width, height);
        let terminal = Terminal::with_options(
            TtySized {
                inner: backend,
                size: area.as_size(),
                open: false,
                emit: B::emit,
                touched: false,
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
            look: Look::default(),
        })
    }

    /// Paints every later frame with `look`, carrying the last reported
    /// appearance into it so a picked theme keeps following the terminal;
    /// the next draw repaints whole.
    pub(crate) fn set_look(&mut self, mut look: Look) {
        look.appearance(self.look.reported());
        self.look = look;
        self.last = None;
    }

    /// Records the terminal's reported `appearance`, repainting whole
    /// when the colours changed (`docs/tui.md`, "Themes").
    pub(crate) fn appearance(&mut self, appearance: Appearance) {
        if self.look.appearance(appearance) {
            self.last = None;
        }
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
        let mut cells = themed(self.area);
        self.targets = render(app, self.area, &mut cells, pointer);
        if app.drawn(&self.targets) {
            cells = themed(self.area);
            self.targets = render(app, self.area, &mut cells, pointer);
            let _ = app.drawn(&self.targets);
        }
        let next = (cells, view::cursor(app, self.area));
        if self.last.as_ref() == Some(&next) {
            return Ok(());
        }
        let look = &self.look;
        self.terminal.draw(|frame| {
            frame.buffer_mut().clone_from(&next.0);
            look.paint(frame.buffer_mut());
            if let Some(cursor) = next.1 {
                frame.set_cursor_position(cursor);
            }
        })?;
        self.last = Some(next);
        Ok(())
    }

    /// Resizes the viewport; the next draw repaints it whole. The resize's
    /// own bytes reach the tty before returning, with the block left open
    /// for the redraw, as below.
    pub(crate) fn resize(&mut self, width: u16, height: u16) -> Result<(), B::Error> {
        self.area = Rect::new(0, 0, width, height);
        self.last = None;
        self.terminal.backend_mut().size = self.area.as_size();
        self.terminal.resize(self.area)?;
        self.terminal.backend_mut().flush_inner()
    }

    /// Clears the viewport for an immediate redraw in the same step: a
    /// shown logo image lost its rect, and the redraw must follow at
    /// once, never a blank screen (`docs/tui.md`, "The logo").
    pub(crate) fn repaint(&mut self) -> Result<(), B::Error> {
        self.terminal.clear()?;
        self.last = None;
        Ok(())
    }

    /// Whether the last frame wrote bytes, clearing the mark: the
    /// logo's positioned image rewrites after any touched frame
    /// (`docs/tui.md`, "The logo").
    pub(crate) fn take_touched(&mut self) -> bool {
        let backend = self.terminal.backend_mut();
        let touched = backend.touched;
        backend.touched = false;
        touched
    }

    /// The logo's tint and page background, while the look has colour
    /// (`docs/tui.md`, "The logo").
    pub(crate) fn logo_tint(&self) -> Option<(crate::graphics::logo::Tint, crate::theme::Rgb)> {
        self.look.logo_tint()
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

impl<B: Backend> Drop for Screen<B> {
    /// Shows the cursor through the terminal before it drops, closing
    /// the synchronized-output block: ratatui's `Drop for Terminal`
    /// restores a hidden cursor with a bare write, outside any block
    /// and after the terminal is restored. Best effort: the terminal is
    /// already restored on the way out.
    fn drop(&mut self) {
        self.terminal.show_cursor().unwrap_or(());
        self.terminal.backend_mut().flush().unwrap_or(());
    }
}

/// A blank frame on the theme's text and background colours, so text drawn
/// with no colour of its own takes the theme's (`docs/tui.md`, "Themes").
fn themed(area: Rect) -> Buffer {
    let mut blank = Cell::default();
    blank.set_style(
        Style::new()
            .fg(Role::Text.color())
            .bg(Role::Background.color()),
    );
    Buffer::filled(area, blank)
}

/// Synchronized output's begin marker (DEC mode 2026): the first byte of a
/// frame. The same bytes crossterm's `BeginSynchronizedUpdate` writes; they
/// are written here so no new dependency is needed. A terminal without
/// support ignores the sequence (`docs/tui.md`, "Performance").
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
/// Synchronized output's end marker (DEC mode 2026): the last byte of a
/// frame, written on flush.
const SYNC_END: &[u8] = b"\x1b[?2026l";

/// How one synchronized-output marker reaches the terminal: backends over
/// a byte sink write mode 2026's markers, every other backend ignores
/// them. `TestBackend` has no `Write`, so the constructor takes this
/// bound instead of one on it.
pub(crate) trait SyncEmit: Backend {
    fn emit(&mut self, begin: bool) -> Result<(), Self::Error>;
}

impl<W: std::io::Write> SyncEmit for CrosstermBackend<W> {
    fn emit(&mut self, begin: bool) -> Result<(), Self::Error> {
        use std::io::Write as _;
        self.write_all(if begin { SYNC_BEGIN } else { SYNC_END })
    }
}

/// One frame's bytes on their way to the tty: every byte crossterm
/// writes waits here. Crossterm's cursor commands flush eagerly
/// (`execute!`), mid-frame; `flush` holds those: the frame backend
/// below delivers the whole buffer in one `write` when the frame ends. Once a `Screen` call returns `Ok`, every byte it
/// produced has reached the tty, in order; once it returns `Err`, nothing
/// it buffered is ever written later. It never writes on drop, so no
/// byte can reach the tty after the restore bytes at exit.
pub(crate) struct FrameWriter<W> {
    shared: std::rc::Rc<std::cell::RefCell<FrameWriterInner<W>>>,
}

impl<W> Clone for FrameWriter<W> {
    fn clone(&self) -> Self {
        Self {
            shared: std::rc::Rc::clone(&self.shared),
        }
    }
}

/// The tty and one frame's bytes, shared between the writer crossterm
/// sees and the frame backend draining it.
struct FrameWriterInner<W> {
    out: W,
    buf: Vec<u8>,
}

/// One frame's reserved bytes: picked, not measured.
// debt: 65,536 is picked, not measured; a measured frame size on the
// benchmark runner would set it.
const FRAME_BUFFER: usize = 65_536;

impl<W: std::io::Write> FrameWriter<W> {
    /// Ready to buffer `out`'s frames.
    fn new(out: W) -> Self {
        Self {
            shared: std::rc::Rc::new(std::cell::RefCell::new(FrameWriterInner {
                out,
                buf: Vec::with_capacity(FRAME_BUFFER),
            })),
        }
    }
}

impl<W> FrameWriterInner<W>
where
    W: std::io::Write,
{
    /// Writes the buffered bytes in one `write_all`, then flushes, and
    /// empties the buffer whether that succeeds or fails. Only the frame
    /// backend calls this, once per frame: an empty buffer writes nothing.
    fn drain(&mut self) -> std::io::Result<()> {
        let drained = if self.buf.is_empty() {
            Ok(())
        } else {
            self.out.write_all(&self.buf)
        };
        self.buf.clear();
        drained.and(self.out.flush())
    }
}

impl<W: std::io::Write> std::io::Write for FrameWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.shared.borrow_mut().buf.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    /// Holds: crossterm's mid-frame flushes deliver nothing; the frame
    /// backend drains the buffer at the frame's end.
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Crossterm over the frame writer, with a `flush` that delivers the
/// whole buffered frame in one write. Crossterm flushes eagerly around
/// its cursor commands, mid-frame; those flushes only reach the writer
/// above, which holds them. The frame's end, and the resize below, drain
/// through here instead, so each draws exactly one `write`.
pub(crate) struct FrameBackend<W: std::io::Write, B = CrosstermBackend<FrameWriter<W>>> {
    inner: B,
    writer: FrameWriter<W>,
}

impl<W, B> Backend for FrameBackend<W, B>
where
    W: std::io::Write,
    B: Backend<Error = std::io::Error>,
{
    type Error = std::io::Error;

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
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.writer.shared.borrow_mut().drain()
    }
}

impl<W: std::io::Write> SyncEmit for FrameBackend<W> {
    fn emit(&mut self, begin: bool) -> Result<(), Self::Error> {
        use std::io::Write as _;
        self.inner
            .write_all(if begin { SYNC_BEGIN } else { SYNC_END })
    }
}

/// A crossterm backend writing each frame in one write, through the frame
/// writer.
pub(crate) fn backend<W: std::io::Write>(out: W) -> FrameBackend<W> {
    let writer = FrameWriter::new(out);
    FrameBackend {
        inner: CrosstermBackend::new(writer.clone()),
        writer,
    }
}

impl SyncEmit for TestBackend {
    fn emit(&mut self, _begin: bool) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// A backend that reports the size read from the injected tty. ratatui
/// asks its backend for the size when it clears a fixed viewport on
/// resize, and crossterm answers from `/dev/tty`, standard output or
/// `tput`, never from the injected tty: with none of those, as under a
/// test harness, the answer is an error and the resize fails.
///
/// It also brackets every frame in synchronized output: the begin marker
/// precedes the frame's first byte and the end marker is written on
/// flush, so the terminal shows only whole frames. The block stays open
/// across calls, so a resize's clear and the redraw that follows share
/// one block; blocks never nest.
struct TtySized<B: Backend> {
    inner: B,
    size: Size,
    /// Whether a synchronized-output block is open: the begin marker went
    /// out and no flush closed it yet.
    open: bool,
    /// Writes one synchronized-output marker to the inner backend.
    emit: fn(&mut B, begin: bool) -> Result<(), B::Error>,
    /// Whether the last frame wrote bytes: the logo's positioned image
    /// rewrites after any touched frame (`docs/tui.md`, "The logo").
    touched: bool,
}

impl<B: Backend> TtySized<B> {
    /// Opens the frame's synchronized-output block, unless one is open.
    fn open_block(&mut self) -> Result<(), B::Error> {
        if !self.open {
            let emit = self.emit;
            emit(&mut self.inner, true)?;
            self.open = true;
        }
        Ok(())
    }

    /// Flushes the inner backend without closing the block: after a
    /// resize's clear, so its bytes are on the wire before returning
    /// while the redraw still joins their block.
    fn flush_inner(&mut self) -> Result<(), B::Error> {
        self.inner.flush()
    }
}

impl<B: Backend> Backend for TtySized<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut content = content.peekable();
        if content.peek().is_some() {
            self.touched = true;
        }
        self.open_block()?;
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.open_block()?;
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.open_block()?;
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.open_block()?;
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.touched = true;
        self.open_block()?;
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.touched = true;
        self.open_block()?;
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
        if self.open {
            let emit = self.emit;
            emit(&mut self.inner, false)?;
            self.open = false;
        }
        self.inner.flush()
    }
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
