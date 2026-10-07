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
mod clipboard;
mod editor;
mod files;
mod focus;
mod format;
mod highlight;
mod home;
mod input;
mod jigs;
mod keymap;
mod keys;
mod layout;
mod link;
mod markdown;
mod mouse;
mod offer;
mod osc;
mod pages;
mod shell;
mod slash;
mod term;
mod turn;
mod turn_text;
mod view;
mod window;

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, PipeReader, PipeWriter, Write};
use std::ops::RangeInclusive;
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;

use contract::clock::Clock;
use contract::{Envelope, HubLine, Seq, SessionId};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Rect, Size};
use ratatui::{Terminal, TerminalOptions, Viewport};
use signal_hook::consts::{SIGINT, SIGQUIT, SIGWINCH};
use signal_hook::iterator::Signals;

use crate::app::{App, Effect, mint, session_command};
use crate::keys::{Button, Event, MouseKind, Parser, Reply};
use crate::link::Line;
use crate::mouse::{Pointer, Target};

pub use home::Launch;

pub use jigs::{draw, hover_frames, measure_paging};

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

/// Runs the terminal on `tty` for `launch`, starting sessions in its
/// workspace. `hover` is `launch.hover`: with it off, mouse mode 1003 is
/// never sent and nothing is tinted under the pointer. Returns 0 on quit
/// and 1 when the terminal cannot be set up or drawn. The terminal is
/// restored on every return.
pub fn run(
    tty: File,
    launch: Launch,
    connect: Connect,
    on_attach: OnAttach,
    clock: Arc<dyn Clock>,
) -> i32 {
    // SIGWINCH is caught from before the size is read, so no resize is
    // missed; its thread starts after the first frame.
    let signals = Signals::new([SIGWINCH]).ok();
    catch_interrupts();
    let restore = term::Guard;
    // Home is always set, so the first frame draws at once: it runs no
    // child process and reads nothing.
    let hover = launch.hover;
    let Ok((width, height)) = term::setup(&tty, hover) else {
        return 1;
    };
    let Ok(out) = tty.try_clone() else {
        return 1;
    };
    let Ok(screen) = Screen::new(CrosstermBackend::new(out), width, height) else {
        return 1;
    };
    let mut app = App::new(launch.workspace.clone());
    app.set_home(launch);
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
        reader: None,
        pointer: Pointer::default(),
        hover,
        var: Box::new(|name| std::env::var(name).ok()),
        copy_command: clipboard::command(|name| std::env::var_os(name), clipboard::on_path)
            .map(|argv| argv.into_iter().map(str::to_owned).collect()),
        title: osc::Title::default(),
    };
    // The first frame waits on nothing: the queries are out, and nothing
    // reads the tty or the hub until it is drawn.
    if terminal.screen.draw(&mut terminal.app, None).is_err() {
        return 1;
    }
    terminal.write_title();
    let (tx, rx) = mpsc::channel();
    terminal.files_out = Some(tx.clone());
    terminal.reader = terminal
        .tty
        .as_ref()
        .and_then(|tty| Reader::spawn(tty, tx.clone()));
    spawn_hub(connect, tx.clone());
    if let Some(signals) = signals {
        spawn_resize(signals, tx);
    }
    let code = terminal.run(&rx);
    // One resume line per live session: collected first, then the hub
    // hangs up, the terminal is restored, and the lines print in cooked
    // mode, each ending in a newline.
    let lines = terminal.app.exit_lines();
    // Ends the hub reader thread; the input and resize threads stay
    // blocked and end with the process.
    terminal.hang_up();
    drop(restore);
    if let Some(mut tty) = terminal.tty.take() {
        for line in &lines {
            writeln!(tty, "{line}").unwrap_or(());
        }
    }
    code
}

/// Catches SIGINT and SIGQUIT for the rest of the process's life, so `Ctrl+C`
/// or `Ctrl+\` typed while the editor has the terminal in cooked mode ends
/// only the editor. In raw mode the tty sends neither. Never unregistered:
/// unregistering does not restore the default disposition. The editor,
/// after `exec`, has the default one.
fn catch_interrupts() {
    let caught = Arc::new(AtomicBool::new(false));
    for signal in [SIGINT, SIGQUIT] {
        match signal_hook::flag::register(signal, Arc::clone(&caught)) {
            Ok(_) | Err(_) => {}
        }
    }
}

/// Restores the terminal [`run`] set up: turns mouse reporting off, leaves
/// the alternate screen, shows the cursor and restores the saved terminal
/// modes. Idempotent, takes no
/// lock, and does nothing when [`run`] never set the terminal up. The panic
/// hook calls it first.
pub fn restore() {
    term::restore();
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
    /// targets. When the frame drops the focused target, focus returns to
    /// the input box and the frame is drawn again, so the cursor shows.
    /// A frame whose cells and cursor equal the last one's writes
    /// nothing; otherwise only the cells that changed are written.
    fn draw(&mut self, app: &mut App, pointer: Option<(u16, u16)>) -> Result<(), B::Error> {
        self.draw_with(app, pointer, view::render)
    }

    fn draw_with(
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

/// Reads an environment variable by name.
type Var = Box<dyn Fn(&str) -> Option<String> + Send>;

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
    /// The tty's reader, paused while the editor has the terminal.
    reader: Option<Reader>,
    /// The pointer's last cell and a pending click.
    pointer: Pointer,
    /// `tui.hover`: whether the pointer's cell is recorded and tinted.
    hover: bool,
    /// Reads an environment variable: `$VISUAL` and `$EDITOR` for Ctrl+G.
    var: Var,
    /// The system clipboard command a copy is piped to, beside OSC 52.
    copy_command: Option<Vec<String>>,
    /// The window title last written.
    title: osc::Title,
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
        let before = self.app.session().cloned();
        match input {
            Input::Bytes(bytes) => {
                for event in self.parser.feed(&bytes) {
                    let effect = match event {
                        Event::Key(key) => self.app.on_key(key, self.clock.now()),
                        Event::Edit(edit) => self.app.on_edit(edit),
                        Event::Mouse(mouse) => {
                            // Every left click, on a target or not, clears
                            // "Copied"; a click on `copy` sets it again.
                            if mouse.kind == MouseKind::Press(Button::Left) {
                                self.app.clear_copied();
                            }
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
                        Effect::Copy(text) => {
                            clipboard::copy(self.tty.as_ref(), self.copy_command.as_deref(), text);
                        }
                        Effect::Send(lines) => self.send(&lines),
                        Effect::Quit => return Some(0),
                        Effect::Exit(lines) => {
                            // The quit question's closes go out, then the
                            // terminal quits: a close never written keeps
                            // its resume line.
                            self.send(&lines);
                            return Some(0);
                        }
                        Effect::ListFiles => self.list_files(),
                        Effect::Search { generation, query } => {
                            if let Some(search) = &self.search {
                                search.search(generation, query);
                            }
                        }
                        Effect::Editor { target, text } => {
                            if let Some(code) = self.open_editor(target, &text) {
                                return Some(code);
                            }
                        }
                    }
                }
            }
            Input::Hub(line) => {
                let lines = self.app.on_line(line);
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
        // An attach the input brought is reported once: the session
        // changed to one, from none.
        if before.is_none()
            && let Some(session) = self.app.session()
        {
            (self.on_attach)(session);
        }
        // A closed `@` panel drops its worker and the listing it holds.
        if !self.app.files_open() {
            self.search = None;
        }
        self.page_in(rx);
        if self.screen.draw(&mut self.app, self.pointer.at).is_err() {
            return Some(1);
        }
        self.write_title();
        None
    }

    /// Writes the app's window title when it changed since the last write.
    fn write_title(&mut self) {
        if let Some(bytes) = self.title.next(self.app.title())
            && let Some(mut tty) = self.tty.as_ref()
        {
            tty.write_all(&bytes).unwrap_or(());
        }
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
        let workspace = self.app.workspace();
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

    /// Ctrl+G: opens `text` in the editor `$VISUAL` or `$EDITOR` names,
    /// with the terminal handed over, and gives the app what it returned.
    /// `Some(1)` when the terminal cannot be taken back.
    fn open_editor(&mut self, target: editor::Target, text: &str) -> Option<i32> {
        let Some(command) = editor::command(&self.var) else {
            self.app
                .editor_returned(target, Err(editor::NO_EDITOR.to_owned()));
            return None;
        };
        let mut result = Err(String::new());
        let code = self.hand_over(|| result = editor::run(&command, text));
        if code.is_none() {
            self.app.editor_returned(target, result);
        }
        code
    }

    /// Hands the terminal to `program`, run in the foreground on this
    /// thread: the reader paused, the terminal restored, then both taken
    /// back and the whole screen repainted at the size read again. `Some(1)`
    /// when the terminal cannot be taken back.
    fn hand_over(&mut self, program: impl FnOnce()) -> Option<i32> {
        if let Some(reader) = &mut self.reader {
            reader.pause();
        }
        // A failed suspend still runs the program: the terminal may be
        // left part set up, and resume sets it up whole.
        term::suspend().unwrap_or(());
        program();
        if term::resume(self.parser.kitty(), self.hover).is_err() {
            return Some(1);
        }
        // The restore popped the title: the next frame writes it again.
        self.title.forget();
        if let Some(reader) = &self.reader {
            reader.resume();
        }
        let (width, height) = match self.tty.as_ref().map(term::size) {
            Some(Ok(size)) => size,
            Some(Err(_)) | None => (self.screen.area.width, self.screen.area.height),
        };
        self.app.set_size(width, height);
        self.screen.resize(width, height).err().map(|_| 1)
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

/// The input reader's state, shared with the loop under one mutex.
#[derive(Debug, Default)]
struct ReaderState {
    /// The loop asked the reader to stop reading the tty.
    paused: bool,
    /// The reader stopped and waits for `paused` to clear.
    parked: bool,
    /// The reader returned, or never started: nothing to wait for.
    ended: bool,
}

/// The pause handshake: the state and the condition variable each change
/// is announced on.
#[derive(Debug, Default)]
struct Gate {
    state: Mutex<ReaderState>,
    changed: Condvar,
}

impl Gate {
    /// The state, even after a thread panicked holding it.
    fn lock(&self) -> MutexGuard<'_, ReaderState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Waits on the condition variable while `blocked` holds.
    fn wait_while<'a>(
        &self,
        guard: MutexGuard<'a, ReaderState>,
        blocked: impl FnMut(&mut ReaderState) -> bool,
    ) -> MutexGuard<'a, ReaderState> {
        self.changed
            .wait_while(guard, blocked)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records that the reader returned.
    fn end(&self) {
        self.lock().ended = true;
        self.changed.notify_all();
    }
}

/// Reads the tty on its own thread, polling it and a wake pipe, so the loop
/// can stop it from reading while another program has the terminal. Left
/// blocked on quit; it ends with the process.
struct Reader {
    gate: Arc<Gate>,
    /// Written to wake the reader from its poll.
    wake: PipeWriter,
}

impl Reader {
    /// Starts reading `tty`, sending each read as [`Input::Bytes`]. `None`
    /// when the reader cannot start: the terminal then reads no keys, the
    /// hub thread may still report, and Ctrl+C from the shell ends it.
    fn spawn(tty: &File, tx: Sender<Input>) -> Option<Self> {
        let tty = tty.try_clone().ok()?;
        let (woken, wake) = io::pipe().ok()?;
        let gate = Arc::new(Gate::default());
        let shared = Arc::clone(&gate);
        thread::Builder::new()
            .name("tui-input".to_owned())
            .spawn(move || {
                read_input(tty, &woken, &shared, &tx);
                shared.end();
            })
            .ok()?;
        Some(Self { gate, wake })
    }

    /// Stops the reader from reading the tty, returning once it has parked
    /// or ended. Bytes it read before parking are already sent.
    fn pause(&mut self) {
        let mut state = self.gate.lock();
        state.paused = true;
        drop(state);
        // A failed write leaves a reader blocked in poll; it parks on the
        // next tty byte, or the loop waits for it until it does.
        io::Write::write_all(&mut self.wake, &[0]).unwrap_or(());
        let state = self.gate.lock();
        drop(
            self.gate
                .wait_while(state, |state| !state.parked && !state.ended),
        );
    }

    /// Lets a paused reader read the tty again.
    fn resume(&self) {
        self.gate.lock().paused = false;
        self.gate.changed.notify_all();
    }
}

/// The reader thread: polls `tty` and `woken`; after every wake drains the
/// pipe, parks while paused, and otherwise reads the tty. Returns on the
/// tty's end, a failed read or poll, or a closed channel.
fn read_input(mut tty: File, mut woken: &PipeReader, gate: &Gate, tx: &Sender<Input>) {
    use rustix::event::{PollFd, PollFlags, poll};
    let mut buf = [0u8; 4096];
    loop {
        let mut fds = [
            PollFd::new(&tty, PollFlags::IN),
            PollFd::new(woken, PollFlags::IN),
        ];
        match poll(&mut fds, None) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => return,
        }
        let [tty_ready, wake_ready] = fds.map(|fd| !fd.revents().is_empty());
        if wake_ready && io::Read::read(&mut woken, &mut buf).is_err() {
            return;
        }
        let mut state = gate.lock();
        if state.paused {
            state.parked = true;
            gate.changed.notify_all();
            state = gate.wait_while(state, |state| state.paused);
            state.parked = false;
            continue;
        }
        drop(state);
        if !tty_ready {
            continue;
        }
        let Ok(read) = io::Read::read(&mut tty, &mut buf) else {
            return;
        };
        let Some(bytes) = buf.get(..read).filter(|bytes| !bytes.is_empty()) else {
            return;
        };
        if tx.send(Input::Bytes(bytes.to_vec())).is_err() {
            return;
        }
    }
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
#[path = "lib_focus_tests.rs"]
mod focus_tests;

#[cfg(test)]
#[path = "loop_tests.rs"]
mod loop_tests;

#[cfg(test)]
#[path = "lib_mouse_tests.rs"]
mod mouse_tests;
