//! The event loop (`docs/tui.md`, "History and paging"): `run` sets the
//! terminal up, draws the first frame and starts the input threads; the loop
//! handles one input at a time, fetches the history pages a frame needs
//! before drawing it, and hands the terminal to the editor.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Write};
use std::ops::RangeInclusive;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};

use contract::clock::Clock;
use contract::{Envelope, Seq, SessionId};
use ratatui::backend::{Backend, CrosstermBackend};
use signal_hook::consts::{SIGINT, SIGQUIT, SIGWINCH};
use signal_hook::iterator::Signals;

use crate::app::{App, Effect, mint, session_command};
use crate::home::Launch;
use crate::keys::{Button, Event, MouseKind, Parser, Reply};
use crate::link::{self, Line};
use crate::look::Look;
use crate::mouse::Pointer;
use crate::screen::Screen;
use crate::sources::{Reader, spawn_hub, spawn_resize};
use crate::{Connect, Input, OnAttach, clipboard, editor, files, osc, term};

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
    let Ok(mut screen) = Screen::new(CrosstermBackend::new(out), width, height) else {
        return 1;
    };
    let mut app = App::new(launch.workspace.clone());
    app.set_zone(jiff::tz::TimeZone::system());
    let mut launch = launch;
    let (look, notice) = Look::new(std::mem::take(&mut launch.theme), &|name| {
        std::env::var(name).ok()
    });
    screen.set_look(look);
    if let Some(notice) = notice {
        app.push_notice(notice);
    }
    app.set_keys(std::mem::take(&mut launch.keys));
    app.set_configure(launch.configure.take());
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
        open_command: crate::opener::command(|name| std::env::var_os(name), clipboard::on_path)
            .map(|argv| argv.into_iter().map(str::to_owned).collect()),
        title: osc::Title::default(),
    };
    terminal.app.set_opener(terminal.open_command.is_some());
    terminal
        .app
        .set_osc9(crate::attention::supported(&terminal.var));
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
    /// The program a link click runs with the URL appended, or none over
    /// SSH or with no opener on `PATH` (`docs/tui.md`, "Links").
    open_command: Option<Vec<String>>,
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
                        Event::Stroke(stroke) => self.app.on_press(stroke, self.clock.now()),
                        Event::Key(key) => self.app.on_key(key, self.clock.now()),
                        Event::Edit(edit) => self.app.on_edit(edit),
                        Event::Mouse(mouse) => {
                            // Every left click, on a target or not, clears
                            // "Copied" and ends a finished attention title
                            // (`docs/tui.md`, "Getting the person's
                            // attention"); a click on `copy` sets it again.
                            if mouse.kind == MouseKind::Press(Button::Left) {
                                self.app.clear_copied();
                                self.app.attention_seen();
                            }
                            let selected = self.app.on_select(&mouse, self.screen.targets());
                            let clicked =
                                self.pointer
                                    .on_mouse(&mouse, self.screen.targets(), self.hover);
                            // A click's effect, else the selection's.
                            let effect =
                                clicked.map_or(Effect::None, |target| self.app.on_click(target));
                            if effect == Effect::None {
                                selected
                            } else {
                                effect
                            }
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
                        Effect::OpenLink(url) => {
                            if let Some(argv) = self.open_command.clone() {
                                let mut argv = argv;
                                argv.push(url);
                                drop(clipboard::pipe(argv, String::new()));
                            }
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
                        Effect::FindPause { generation, after } => {
                            if let Some(out) = &self.files_out {
                                let clock = Arc::clone(&self.clock);
                                let out = out.clone();
                                // A pause thread outliving the loop finds
                                // the channel closed and returns.
                                drop(crate::sources::builder("tui-find-pause").spawn(move || {
                                    clock.sleep(after);
                                    drop(out.send(Input::FindDue(generation)));
                                }));
                            }
                        }
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
                        Effect::OpenFile(file) => {
                            if let Some(code) = self.open_file(&file) {
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
            Input::FindDue(generation) => {
                let lines = self.app.find_due(generation);
                self.send(&lines);
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
        self.apply_theme();
        // A selection's copy waiting on dropped pages runs once they load.
        if let Some(text) = self.app.take_copy() {
            clipboard::copy(self.tty.as_ref(), self.copy_command.as_deref(), text);
        }
        if self.screen.draw(&mut self.app, self.pointer.at).is_err() {
            return Some(1);
        }
        self.write_title();
        self.write_alerts();
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

    /// Writes the attention bytes this input queued (`docs/tui.md`,
    /// "Getting the person's attention"). A failed write is dropped.
    fn write_alerts(&mut self) {
        let bytes = self.app.take_alerts();
        if let Some(mut tty) = self.tty.as_ref() {
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
                    return link::history_answer(&line);
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
                | Input::FindDue(_)
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

    /// Ctrl+G on a configuration view: opens `file` in the editor
    /// `$VISUAL` or `$EDITOR` names, with the terminal handed over, and
    /// tells the app it returned. `Some(1)` when the terminal cannot be
    /// taken back.
    fn open_file(&mut self, file: &std::path::Path) -> Option<i32> {
        let Some(command) = editor::command(&self.var) else {
            self.app
                .config_file_closed(Err(editor::NO_EDITOR_FILE.to_owned()));
            return None;
        };
        let mut result = Err(String::new());
        let code = self.hand_over(|| result = editor::open(&command, file));
        if code.is_none() {
            self.app.config_file_closed(result);
        }
        code
    }

    /// Applies a theme `/settings` chose, without a restart: the next
    /// frame is painted whole in it, keeping the colour depth. A theme
    /// file that cannot be read follows the terminal, with its notice
    /// (`docs/tui.md`, "Themes").
    fn apply_theme(&mut self) {
        let Some(setting) = self.app.take_theme_choice() else {
            return;
        };
        let (look, notice) = Look::new(setting, &|name| (self.var)(name));
        self.screen.set_look(look);
        if let Some(notice) = notice {
            self.app.push_notice(notice);
        }
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
            Some(Err(_)) | None => (self.screen.area().width, self.screen.area().height),
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

#[cfg(test)]
#[path = "lib_attention_tests.rs"]
mod attention_tests;

#[cfg(test)]
#[path = "lib_look_tests.rs"]
mod look_tests;
