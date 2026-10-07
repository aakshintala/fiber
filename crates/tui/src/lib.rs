//! The terminal: one session on screen through the hub (`docs/tui.md`).
//!
//! [`run`] sets the injected tty up, draws the first frame, then starts the
//! threads that feed one loop: terminal bytes, hub lines and resizes. The
//! loop's only wait is a channel receive with no timeout, so nothing runs
//! while nothing happens.

mod app;
mod keys;
mod link;
mod term;
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
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use signal_hook::consts::SIGWINCH;
use signal_hook::iterator::Signals;

use crate::app::{App, Effect};
use crate::keys::{Event, Parser, Reply};
use crate::link::Line;

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
}

/// Runs the terminal on `tty`, starting sessions in `workspace`. Returns 0
/// on quit and 1 when the terminal cannot be set up or drawn. The terminal
/// is restored on every return.
pub fn run(
    tty: File,
    workspace: PathBuf,
    connect: Connect,
    on_attach: OnAttach,
    clock: Arc<dyn Clock>,
) -> i32 {
    // SIGWINCH is caught from before the size is read, so no resize is
    // missed; its thread starts after the first frame.
    let signals = Signals::new([SIGWINCH]).ok();
    let _restore = term::Guard;
    let Ok((width, height)) = term::setup(&tty) else {
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
    };
    // The first frame waits on nothing: the queries are out, and nothing
    // reads the tty or the hub until it is drawn.
    if terminal.screen.draw(&terminal.app).is_err() {
        return 1;
    }
    let (tx, rx) = mpsc::channel();
    if let Some(tty) = &terminal.tty {
        spawn_input(tty, tx.clone());
    }
    spawn_hub(connect, tx.clone());
    if let Some(signals) = signals {
        spawn_resize(signals, tx);
    }
    let code = terminal.run(&rx);
    if let Some(hub) = terminal.hub.take() {
        // Ends the hub reader thread; the input and resize threads stay
        // blocked and end with the process.
        hub.shutdown(std::net::Shutdown::Both).unwrap_or(());
    }
    code
}

/// Restores the terminal [`run`] set up: leaves the alternate screen, shows
/// the cursor and restores the saved terminal modes. Idempotent, takes no
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
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    view::render(&app, area, &mut buf);
    Ok(view::text(&buf))
}

/// The screen: ratatui on a fixed viewport, and the last frame drawn.
struct Screen<B: Backend> {
    terminal: Terminal<B>,
    area: Rect,
    last: Option<Buffer>,
}

impl<B: Backend> Screen<B> {
    fn new(backend: B, width: u16, height: u16) -> Result<Self, B::Error> {
        let area = Rect::new(0, 0, width, height);
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(area),
            },
        )?;
        Ok(Self {
            terminal,
            area,
            last: None,
        })
    }

    /// Draws `app`. A frame equal to the last one writes nothing; otherwise
    /// only the cells that changed are written.
    fn draw(&mut self, app: &App) -> Result<(), B::Error> {
        let mut next = Buffer::empty(self.area);
        view::render(app, self.area, &mut next);
        if self.last.as_ref() == Some(&next) {
            return Ok(());
        }
        self.terminal
            .draw(|frame| frame.buffer_mut().clone_from(&next))?;
        self.last = Some(next);
        Ok(())
    }

    /// Resizes the viewport; the next draw repaints it whole.
    fn resize(&mut self, width: u16, height: u16) -> Result<(), B::Error> {
        self.area = Rect::new(0, 0, width, height);
        self.last = None;
        self.terminal.resize(self.area)
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
                    match event {
                        Event::Key(key) => match self.app.on_key(key, self.clock.now()) {
                            Effect::None => {}
                            Effect::Send(lines) => self.send(&lines),
                            Effect::Quit => return Some(0),
                        },
                        Event::Reply(Reply::KittyFlags(_)) => self.app.set_kitty(),
                        Event::Reply(Reply::DeviceAttributes) => {}
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
            }
            Input::ConnectFailed(error) => {
                self.app
                    .connect_failed(format!("Could not reach the hub: {error}"));
            }
            Input::Disconnected => {
                self.hub = None;
                self.app.disconnected();
            }
            Input::Resize => {
                if let Some(Ok((width, height))) = self.tty.as_ref().map(term::size) {
                    self.app.set_size(width, height);
                    if self.screen.resize(width, height).is_err() {
                        return Some(1);
                    }
                }
            }
        }
        if self.screen.draw(&self.app).is_err() {
            return Some(1);
        }
        None
    }

    /// Writes command lines to the hub. A failed write drops the stream;
    /// the reader then sees the end and reports the connection lost.
    fn send(&mut self, lines: &[String]) {
        for line in lines {
            let Some(hub) = &mut self.hub else {
                return;
            };
            if link::write_line(hub, line).is_err() {
                self.hub = None;
            }
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
