//! The terminal: one session on screen through the hub (`docs/tui.md`).
//!
//! [`run`] sets the injected tty up, draws the first frame, then starts the
//! threads that feed one loop: terminal bytes, hub lines and resizes. The
//! loop's only wait is a channel receive with no timeout, so nothing runs
//! while nothing happens. A frame that needs a page of history it dropped
//! fetches it with `history` and waits for the answer on the same channel
//! before it draws.

mod app;
mod approvals;
mod bindings;
mod files;
mod format;
mod keymap;
mod keys;
mod link;
mod pages;
mod slash;
mod term;
mod turn;
mod view;
mod window;

use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::ops::RangeInclusive;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use contract::{Envelope, HubLine, Seq, SessionId};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Rect, Size};
use ratatui::{Terminal, TerminalOptions, Viewport};
use signal_hook::consts::SIGWINCH;
use signal_hook::iterator::Signals;

use crate::app::{App, Effect, mint, session_command};
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
        files_out: None,
        search: None,
        stash: VecDeque::new(),
    };
    // The first frame waits on nothing: the queries are out, and nothing
    // reads the tty or the hub until it is drawn.
    if terminal.screen.draw(&terminal.app).is_err() {
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

/// Opens `events`, one envelope per line as one session's stream, at
/// `width` by `height` through the terminal's own paging, with `history`
/// answered from the events in memory as the hub answers from the log. The
/// lines after the last `turn_completed` are a turn still running: the
/// rest is opened in one pass, then the jig pages to the top, jumps across
/// the session, changes the width, and appends the running turn one line a
/// frame, and reports what it measured. The `paging` jig prints it
/// (`docs/testing.md`, "Jigs").
pub fn measure_paging(
    events: &str,
    width: u16,
    height: u16,
    clock: Arc<dyn Clock>,
) -> Result<String, String> {
    const JUMPS: usize = 20;
    let started = clock.now();
    let raw: Vec<&str> = events
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    // The running turn follows the last `turn_completed`.
    let running = raw
        .iter()
        .rposition(|line| line.contains(r#""kind":"turn_completed""#))
        .map_or(0, |at| at.saturating_add(1));
    let mut paging = Paging {
        app: App::new(PathBuf::new()),
        log: Vec::new(),
        area: Rect::new(0, 0, width, height),
        clock: Arc::clone(&clock),
        most: 0,
    };
    paging.app.set_size(width, height);
    let (mut turns, mut calls) = (0usize, 0usize);
    let mut tail = Vec::new();
    for (at, line) in events
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
    {
        let envelope: Envelope = serde_json::from_str(line)
            .map_err(|error| format!("line {}: {error}", at.saturating_add(1)))?;
        if let Some(seq) = envelope.seq {
            paging.log.push((seq, line));
        }
        if at >= running {
            tail.push(envelope);
            continue;
        }
        if paging.app.session().is_none() {
            paging.app.attach(envelope.session_id.clone());
        }
        turns = turns.saturating_add(usize::from(envelope.kind == "turn_started"));
        calls = calls.saturating_add(usize::from(envelope.kind == "tool_call_requested"));
        paging.app.on_line(Line::Session(envelope));
        paging.most = paging.most.max(paging.app.pages().resident());
    }
    paging.frame()?;
    let open = clock.now().saturating_duration_since(started);
    let (mut loads, mut slowest_load) = (0usize, Duration::ZERO);
    while paging.app.scroll().0 > 0 {
        paging.app.on_key(keys::Key::PageUp, clock.now());
        let (took, loaded) = paging.frame()?;
        if loaded {
            loads = loads.saturating_add(1);
            slowest_load = slowest_load.max(took);
        }
    }
    let total = paging.app.scroll().1;
    let mut slowest_jump = Duration::ZERO;
    for at in 0..JUMPS {
        paging.app.jump(total.saturating_mul(at) / JUMPS);
        slowest_jump = slowest_jump.max(paging.frame()?.0);
    }
    let mut slowest_width = Duration::ZERO;
    for wide in [width.saturating_sub(1), width] {
        paging.app.set_size(wide, height);
        slowest_width = slowest_width.max(paging.frame()?.0);
    }
    paging.app.on_key(keys::Key::End, clock.now());
    paging.frame()?;
    let pages_before = paging.app.pages().index().pages().len();
    paging.most = 0;
    let mut slowest_append = Duration::ZERO;
    let appended = tail.len();
    for envelope in tail.drain(..) {
        paging.app.on_line(Line::Session(envelope));
        slowest_append = slowest_append.max(paging.frame()?.0);
    }
    let ms = |took: Duration| took.as_secs_f64() * 1000.0;
    Ok(format!(
        "lines: {}\nturns: {turns}\ncalls: {calls}\npages: {pages_before}\nrows: {total}\n\
         open pass and first frame: {:.2} ms\n\
         slowest frame that loaded pages: {:.2} ms, of {loads} paging up\n\
         slowest jump frame: {:.2} ms, of {JUMPS}\n\
         slowest re-count at a new width: {:.2} ms\n\
         slowest append frame: {:.2} ms, of {appended}; pages while appending: {} to {}, \
         most resident {}\n",
        paging.log.len(),
        ms(open),
        ms(slowest_load),
        ms(slowest_jump),
        ms(slowest_width),
        ms(slowest_append),
        pages_before,
        paging.app.pages().index().pages().len(),
        paging.most,
    ))
}

/// The paging jig's terminal: the app, and the session's durable lines as
/// the hub's log holds them, by `seq`.
struct Paging<'a> {
    app: App,
    log: Vec<(Seq, &'a str)>,
    area: Rect,
    clock: Arc<dyn Clock>,
    /// The most pages resident after any frame.
    most: usize,
}

impl Paging<'_> {
    /// One frame: loads what it needs from the log and draws. Returns how
    /// long it took and whether it loaded a page.
    fn frame(&mut self) -> Result<(Duration, bool), String> {
        let started = self.clock.now();
        let mut loaded = false;
        while let Some(range) = self.app.needs().into_iter().next() {
            let from = self.log.partition_point(|(seq, _)| seq < range.start());
            let mut lines = Vec::new();
            for (_, line) in self
                .log
                .iter()
                .skip(from)
                .take_while(|(seq, _)| range.contains(seq))
            {
                lines.push(serde_json::from_str(line).map_err(|error| error.to_string())?);
            }
            self.app.load(lines);
            if self.app.needs().first() == Some(&range) {
                return Err(format!(
                    "the log holds no lines {}..={}",
                    range.start().0,
                    range.end().0
                ));
            }
            loaded = true;
        }
        let mut buf = Buffer::empty(self.area);
        view::render(&self.app, self.area, &mut buf);
        self.most = self.most.max(self.app.pages().resident());
        Ok((self.clock.now().saturating_duration_since(started), loaded))
    }
}

/// The screen: ratatui on a fixed viewport, and the last frame drawn.
struct Screen<B: Backend> {
    terminal: Terminal<TtySized<B>>,
    area: Rect,
    last: Option<Buffer>,
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
    /// Inputs that arrived while a frame waited for history, handled next
    /// in arrival order.
    stash: VecDeque<Input>,
}

/// The most lines one `history` answer holds (`docs/invocation.md`,
/// "Driver commands").
const HISTORY_LINES: u64 = 256;

/// What a lost connection fails a fetch with.
const LOST: &str = "connection lost";

impl<B: Backend> Loop<B> {
    /// Handles inputs until one quits, or every sender is gone: those a
    /// frame held while it waited first, then the channel's. The only wait
    /// is `recv` with no timeout.
    fn run(&mut self, rx: &Receiver<Input>) -> i32 {
        loop {
            let input = match self.stash.pop_front() {
                Some(input) => input,
                None => match rx.recv() {
                    Ok(input) => input,
                    Err(_) => return 0,
                },
            };
            self.wakeups = self.wakeups.saturating_add(1);
            if let Some(code) = self.step(input, rx) {
                return code;
            }
        }
    }

    /// Handles one input, loads the pages the frame needs, and draws what
    /// changed. Returns the exit code when the terminal quits.
    fn step(&mut self, input: Input, rx: &Receiver<Input>) -> Option<i32> {
        match input {
            Input::Bytes(bytes) => {
                for event in self.parser.feed(&bytes) {
                    match event {
                        Event::Key(key) => match self.app.on_key(key, self.clock.now()) {
                            Effect::None => {}
                            Effect::Send(lines) => self.send(&lines),
                            Effect::Quit => return Some(0),
                            Effect::ListFiles => self.list_files(),
                            Effect::Search { generation, query } => {
                                if let Some(search) = &self.search {
                                    search.search(generation, query);
                                }
                            }
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
        self.page_in(rx);
        if self.screen.draw(&self.app).is_err() {
            return Some(1);
        }
        None
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

    /// Loads, one page at a time, every page the frame needs and does not
    /// hold (`docs/tui.md`, "History and paging"). With no hub there is
    /// nothing to ask, and the pages draw blank.
    fn page_in(&mut self, rx: &Receiver<Input>) {
        while let Some(range) = self.app.needs().into_iter().next() {
            let Some(session) = self.app.session().cloned() else {
                return;
            };
            if self.hub.is_none() {
                return;
            }
            match self.fetch(rx, &session, &range) {
                Ok(lines) => {
                    self.app.load(lines);
                    // An answer that folded nothing into the page would
                    // leave it needed, and the frame asking forever.
                    if self.app.needs().first() == Some(&range) {
                        self.app
                            .load_failed(&range, "the answer held none of its lines");
                    }
                }
                Err(message) => self.app.load_failed(&range, &message),
            }
        }
    }

    /// Reads `range` with `history`, in commands of at most
    /// [`HISTORY_LINES`] lines, each answered before the next is sent.
    fn fetch(
        &mut self,
        rx: &Receiver<Input>,
        session: &SessionId,
        range: &RangeInclusive<Seq>,
    ) -> Result<Vec<Envelope>, String> {
        let mut lines = Vec::new();
        let mut from = range.start().0;
        while from <= range.end().0 {
            let to = from.saturating_add(HISTORY_LINES - 1).min(range.end().0);
            let id = mint();
            let args = serde_json::json!({"from_seq": from, "to_seq": to});
            let command = session_command(&id, "history", session, Some(args)).to_string();
            let written = self
                .hub
                .as_ref()
                .is_some_and(|hub| link::write_line(hub, &command).is_ok());
            if !written {
                self.lost();
                return Err(LOST.to_owned());
            }
            let answer = self.answer(rx, &id)?;
            let Some(last) = answer.last().and_then(|line| line.seq) else {
                break;
            };
            from = last.0.saturating_add(1);
            lines.extend(answer);
        }
        Ok(lines)
    }

    /// Waits for the answer to command `id`, holding every other input for
    /// after the frame. A lost connection ends the wait.
    fn answer(&mut self, rx: &Receiver<Input>, id: &str) -> Result<Vec<Envelope>, String> {
        loop {
            let Ok(input) = rx.recv() else {
                self.lost();
                return Err(LOST.to_owned());
            };
            match input {
                Input::Hub(Line::Session(line)) if answers(&line, id) => {
                    return if line.kind == "command_accepted" {
                        line.payload
                            .get("result")
                            .and_then(|result| result.get("lines"))
                            .cloned()
                            .and_then(|lines| serde_json::from_value(lines).ok())
                            .ok_or_else(|| "the answer could not be read".to_owned())
                    } else {
                        Err(line
                            .payload
                            .get("message")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("rejected")
                            .to_owned())
                    };
                }
                Input::Disconnected => {
                    self.lost();
                    return Err(LOST.to_owned());
                }
                other @ (Input::Bytes(_)
                | Input::Hub(_)
                | Input::Connected(..)
                | Input::ConnectFailed(_)
                | Input::Resize
                | Input::Files { .. }) => self.stash.push_back(other),
            }
        }
    }

    /// The connection ended during a fetch: nothing more is asked.
    fn lost(&mut self) {
        self.hang_up();
        self.app.disconnected();
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

/// Whether `line` answers command `id`.
fn answers(line: &Envelope, id: &str) -> bool {
    matches!(line.kind.as_str(), "command_accepted" | "command_rejected")
        && line
            .payload
            .get("command_id")
            .and_then(serde_json::Value::as_str)
            == Some(id)
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
#[path = "loop_tests.rs"]
mod loop_tests;
