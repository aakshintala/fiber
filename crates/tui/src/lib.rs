//! The terminal: one session on screen through the hub (`docs/tui.md`).
//!
//! [`run`] sets the injected tty up, draws the first frame, then starts the
//! threads that feed one loop: terminal bytes, hub lines and resizes. The
//! loop's only wait is a channel receive with no timeout, so nothing runs
//! while nothing happens.

mod app;
mod approvals;
mod bindings;
mod files;
mod format;
mod input;
mod keymap;
mod keys;
mod link;
mod mouse;
mod shell;
mod slash;
mod term;
mod turn;
mod view;

use std::fs::File;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use contract::clock::Clock;
use contract::{Envelope, HubLine, SessionId};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Rect, Size};
use ratatui::{Terminal, TerminalOptions, Viewport};
use signal_hook::consts::SIGWINCH;
use signal_hook::iterator::Signals;

use crate::app::{App, Effect};
use crate::keys::{Event, Parser, Reply};
use crate::link::Line;
use crate::mouse::{Pointer, Target};

/// Connects to the hub, starting one when none runs: the stream and the
/// `hub_hello` it spoke first.
pub type Connect = Box<dyn FnOnce() -> io::Result<(UnixStream, HubLine)> + Send>;

/// Called once with the session id when `start` is accepted.
pub type OnAttach = Box<dyn Fn(&SessionId) + Send>;

/// One thing the loop wakes for.
pub(crate) enum Input {
    /// Terminal bytes, one read.
    Bytes(Vec<u8>),
    /// One line from the hub.
    Hub(Line),
    /// The hub connected: the stream to write commands on, and its
    /// `hub_hello`.
    Connected(UnixStream, HubLine),
    /// The hub could not be reached.
    ConnectFailed(String),
    /// The hub connection ended.
    Disconnected,
    /// The terminal was resized.
    Resize,
    /// A file search result for the `@` panel, tagged with the generation
    /// it searched for: matching paths, or why there are none (the listing
    /// failed). A result for a generation no longer current is dropped.
    Files {
        /// The generation searched for.
        generation: u64,
        /// The paths found, or the listing's error.
        result: Result<Vec<String>, String>,
    },
}

/// Runs the terminal on `tty`, starting sessions in `workspace`. `hover`
/// is `tui.hover`: with it off, mouse mode 1003 is never sent and nothing
/// is tinted under the pointer. Returns 0 on quit and 1 when the terminal
/// cannot be set up or drawn. The terminal is restored on every return.
pub fn run(
    tty: File,
    workspace: PathBuf,
    connect: Connect,
    on_attach: OnAttach,
    clock: Arc<dyn Clock>,
    hover: bool,
) -> i32 {
    // SIGWINCH is caught from before the size is read, so no resize is
    // missed; its thread starts after the first frame.
    let signals = Signals::new([SIGWINCH]).ok();
    let _restore = term::Guard;
    let Ok((width, height)) = term::setup(&tty, hover) else {
        return 1;
    };
    let Ok(out) = tty.try_clone() else {
        return 1;
    };
    let Ok(screen) = Screen::new(CrosstermBackend::new(out), width, height) else {
        return 1;
    };
    let mut app = App::new(workspace);
    app.set_size(width, height);
    let mut terminal = Loop {
        app,
        parser: Parser::default(),
        screen,
        hub: None,
        tty: Some(tty),
        on_attach,
        clock,
        wakeups: 0,
        files_out: None,
        search: None,
        pointer: Pointer::default(),
        hover,
    };
    // The first frame waits on nothing: the queries are out, and nothing
    // reads the tty or the hub until it is drawn.
    if terminal.screen.draw(&terminal.app, None).is_err() {
        return 1;
    }
    let (tx, rx) = mpsc::channel();
    terminal.files_out = Some(tx.clone());
    if let Some(tty) = &terminal.tty {
        spawn_input(tty, tx.clone());
    }
    spawn_hub(connect, tx.clone());
    if let Some(signals) = signals {
        spawn_resize(signals, tx);
    }
    let code = terminal.run(&rx);
    // Ends the hub reader thread; the input and resize threads stay
    // blocked and end with the process.
    terminal.hang_up();
    code
}

/// Restores the terminal [`run`] set up: turns mouse reporting off, leaves
/// the alternate screen, shows the cursor and restores the saved terminal
/// modes. Idempotent, takes no
/// lock, and does nothing when [`run`] never set the terminal up. The panic
/// hook calls it first.
pub fn restore() {
    term::restore();
}

/// Folds `events`, one envelope per line as one session's stream, and
/// draws them at `width` by `height`. Returns the screen as text, each row
/// trimmed of trailing spaces. An unreadable line is an error naming its
/// number. The `draw` jig prints it (`docs/testing.md`, "Jigs").
pub fn draw(events: &str, width: u16, height: u16) -> Result<String, String> {
    let app = fold(events, width, height)?;
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view::render(&app, area, &mut buf, None);
    Ok(view::text(&buf))
}

/// Folds `events` as [`draw`] does and puts the request the panel shows
/// aside, so it waits on the badge, a click target. Then draws them at
/// `width` by `height` through the loop's screen, and moves the pointer to
/// each of `pointer` in turn, drawing after each as the loop does for a
/// motion report. Returns the bytes each report wrote. The `hover` jig
/// times it (`docs/tui.md`, "Mouse and hover").
pub fn hover_frames(
    events: &str,
    width: u16,
    height: u16,
    pointer: &[(u16, u16)],
) -> Result<Vec<usize>, String> {
    let mut app = fold(events, width, height)?;
    app.put_aside();
    let written = Counter::default();
    let mut screen = Screen::new(CrosstermBackend::new(written.clone()), width, height)
        .map_err(|error| error.to_string())?;
    screen.draw(&app, None).map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(pointer.len());
    for at in pointer {
        let before = written.0.get();
        screen
            .draw(&app, Some(*at))
            .map_err(|error| error.to_string())?;
        bytes.push(written.0.get().saturating_sub(before));
    }
    Ok(bytes)
}

/// Counts the bytes written through it.
#[derive(Clone, Default)]
struct Counter(std::rc::Rc<std::cell::Cell<usize>>);

impl io::Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.set(self.0.get().saturating_add(bytes.len()));
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// An app at `width` by `height` with `events` folded, one envelope per
/// line as one session's stream. An unreadable line is an error naming
/// its number.
fn fold(events: &str, width: u16, height: u16) -> Result<App, String> {
    let mut app = App::new(PathBuf::new());
    app.set_size(width, height);
    for (at, line) in events.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let envelope: Envelope = serde_json::from_str(line)
            .map_err(|error| format!("line {}: {error}", at.saturating_add(1)))?;
        if app.session().is_none() {
            app.attach(envelope.session_id.clone());
        }
        app.on_line(Line::Session(envelope));
    }
    Ok(app)
}

/// One frame: the cells, and where the cursor shows, if anywhere.
type Frame = (Buffer, Option<Position>);

/// The screen: ratatui on a fixed viewport, and the last frame drawn with
/// its click targets.
struct Screen<B: Backend> {
    terminal: Terminal<TtySized<B>>,
    area: Rect,
    last: Option<Frame>,
    /// The click targets of the last frame drawn: what a click hits.
    targets: Vec<Target>,
}

impl<B: Backend> Screen<B> {
    fn new(backend: B, width: u16, height: u16) -> Result<Self, B::Error> {
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
    /// targets. A frame whose cells and cursor equal the last one's writes
    /// nothing; otherwise only the cells that changed are written.
    fn draw(&mut self, app: &App, pointer: Option<(u16, u16)>) -> Result<(), B::Error> {
        let mut cells = Buffer::empty(self.area);
        self.targets = view::render(app, self.area, &mut cells, pointer);
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
    fn resize(&mut self, width: u16, height: u16) -> Result<(), B::Error> {
        self.area = Rect::new(0, 0, width, height);
        self.last = None;
        self.terminal.backend_mut().size = self.area.as_size();
        self.terminal.resize(self.area)
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

/// The loop's state.
struct Loop<B: Backend> {
    app: App,
    parser: Parser,
    screen: Screen<B>,
    /// The hub stream commands are written on, once connected.
    hub: Option<UnixStream>,
    /// The tty, for its size on resize.
    tty: Option<File>,
    on_attach: OnAttach,
    clock: Arc<dyn Clock>,
    /// Inputs handled, for the idle test.
    wakeups: u64,
    /// Where a file search worker posts its results.
    files_out: Option<Sender<Input>>,
    /// The `@` panel's search worker, while the panel is open.
    search: Option<files::Search>,
    /// The pointer's last cell and a pending click.
    pointer: Pointer,
    /// `tui.hover`: whether the pointer's cell is recorded and tinted.
    hover: bool,
}

impl<B: Backend> Loop<B> {
    /// Handles inputs until one quits, or every sender is gone. The only
    /// wait is `recv` with no timeout.
    fn run(&mut self, rx: &Receiver<Input>) -> i32 {
        while let Ok(input) = rx.recv() {
            self.wakeups = self.wakeups.saturating_add(1);
            if let Some(code) = self.step(input) {
                return code;
            }
        }
        0
    }

    /// Handles one input and draws what changed. Returns the exit code
    /// when the terminal quits.
    fn step(&mut self, input: Input) -> Option<i32> {
        match input {
            Input::Bytes(bytes) => {
                for event in self.parser.feed(&bytes) {
                    let effect = match event {
                        Event::Key(key) => self.app.on_key(key, self.clock.now()),
                        Event::Edit(edit) => self.app.on_edit(edit),
                        Event::Mouse(mouse) => {
                            let clicked =
                                self.pointer
                                    .on_mouse(&mouse, &self.screen.targets, self.hover);
                            clicked.map_or(Effect::None, |target| self.app.on_click(target))
                        }
                        Event::Reply(Reply::KittyFlags(_)) => {
                            self.kitty();
                            Effect::None
                        }
                        Event::Reply(Reply::DeviceAttributes) => Effect::None,
                    };
                    match effect {
                        Effect::None => {}
                        Effect::Send(lines) => self.send(&lines),
                        Effect::Quit => return Some(0),
                        Effect::ListFiles => self.list_files(),
                        Effect::Search { generation, query } => {
                            if let Some(search) = &self.search {
                                search.search(generation, query);
                            }
                        }
                    }
                }
            }
            Input::Hub(line) => {
                let attached = self.app.session().is_some();
                let lines = self.app.on_line(line);
                if !attached && let Some(session) = self.app.session() {
                    (self.on_attach)(session);
                }
                self.send(&lines);
            }
            Input::Connected(stream, hello) => {
                self.hub = Some(stream);
                let lines = self.app.on_line(Line::Hub(hello));
                self.send(&lines);
                // A `hub_hello` this terminal cannot read leaves it
                // unconnected: it says so and disconnects.
                if !self.app.connected() {
                    self.hang_up();
                }
            }
            Input::ConnectFailed(error) => {
                self.app
                    .connect_failed(format!("Could not reach the hub: {error}"));
            }
            Input::Disconnected => {
                self.hub = None;
                self.app.disconnected();
            }
            Input::Files { generation, result } => self.app.on_files(generation, result),
            Input::Resize => {
                if let Some(Ok((width, height))) = self.tty.as_ref().map(term::size) {
                    self.app.set_size(width, height);
                    if self.screen.resize(width, height).is_err() {
                        return Some(1);
                    }
                }
            }
        }
        // A closed `@` panel drops its worker and the listing it holds.
        if !self.app.files_open() {
            self.search = None;
        }
        if self.screen.draw(&self.app, self.pointer.at).is_err() {
            return Some(1);
        }
        None
    }

    /// Kitty's flags reply: the first pushes the flags the bindings need
    /// (`docs/tui.md`, "Keys", "Rules"). A failed write leaves the legacy
    /// keys, which every binding also has.
    fn kitty(&mut self) {
        if self.app.kitty() {
            return;
        }
        self.app.set_kitty();
        if let Some(mut tty) = self.tty.as_ref()
            && io::Write::write_all(&mut tty, term::KITTY_PUSH).is_ok()
        {
            self.parser.set_kitty();
        }
    }

    /// Starts the `@` panel's search worker on a listing of the workspace,
    /// searching for an empty query at the current generation.
    fn list_files(&mut self) {
        let Some(out) = &self.files_out else {
            return;
        };
        let workspace = self.app.workspace().to_path_buf();
        let search = files::Search::spawn(move || files::list(&workspace), out.clone());
        search.search(self.app.generation(), String::new());
        self.search = Some(search);
    }

    /// Writes command lines to the hub. A failed write hangs up: the
    /// connection is lost, and the text of the lines not sent returns to
    /// the draft.
    fn send(&mut self, lines: &[String]) {
        for (at, line) in lines.iter().enumerate() {
            let Some(hub) = &mut self.hub else {
                return;
            };
            if link::write_line(hub, line).is_err() {
                self.hang_up();
                self.app.write_failed(lines.get(at..).unwrap_or_default());
                return;
            }
        }
    }

    /// Shuts the hub stream down both ways and drops it. The reader thread,
    /// on its clone, then sees the end.
    fn hang_up(&mut self) {
        if let Some(hub) = self.hub.take() {
            hub.shutdown(std::net::Shutdown::Both).unwrap_or(());
        }
    }
}

/// Reads the tty on its own thread. Left blocked on quit; it ends with the
/// process.
fn spawn_input(tty: &File, tx: Sender<Input>) {
    let Ok(mut tty) = tty.try_clone() else {
        return;
    };
    let reader = thread::Builder::new()
        .name("tui-input".to_owned())
        .spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(read) = io::Read::read(&mut tty, &mut buf) {
                let Some(bytes) = buf.get(..read).filter(|bytes| !bytes.is_empty()) else {
                    return;
                };
                if tx.send(Input::Bytes(bytes.to_vec())).is_err() {
                    return;
                }
            }
        });
    // A thread that cannot start leaves the terminal unable to read keys;
    // the hub thread may still report, and Ctrl+C from the shell ends it.
    drop(reader);
}

/// Connects to the hub on its own thread, after the first frame, then
/// reads its lines.
fn spawn_hub(connect: Connect, tx: Sender<Input>) {
    let hub = thread::Builder::new()
        .name("tui-hub".to_owned())
        .spawn(move || {
            let connected = connect().and_then(|(stream, hello)| {
                let reader = stream.try_clone()?;
                Ok((stream, reader, hello))
            });
            match connected {
                Ok((stream, reader, hello)) => {
                    if tx.send(Input::Connected(stream, hello)).is_ok() {
                        link::read_lines(reader, &tx);
                    }
                }
                Err(error) => drop(tx.send(Input::ConnectFailed(error.to_string()))),
            }
        });
    drop(hub);
}

/// Turns SIGWINCH into [`Input::Resize`] on its own thread. Left blocked on
/// quit; it ends with the process.
fn spawn_resize(mut signals: Signals, tx: Sender<Input>) {
    let resize = thread::Builder::new()
        .name("tui-resize".to_owned())
        .spawn(move || {
            for _ in signals.forever() {
                if tx.send(Input::Resize).is_err() {
                    return;
                }
            }
        });
    drop(resize);
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lib_mouse_tests.rs"]
mod mouse_tests;
