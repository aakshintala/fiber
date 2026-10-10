//! The event loop (`docs/tui.md`, "History and paging"): `run` sets the
//! terminal up, draws the first frame and starts the input threads; the loop
//! handles one input at a time, fetches the history pages a frame needs
//! before drawing it, and hands the terminal to the editor.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Write};
use std::ops::RangeInclusive;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};

use contract::clock::Clock;
use contract::{Envelope, Seq, SessionId};
use ratatui::backend::{Backend, CrosstermBackend};
use signal_hook::consts::{SIGINT, SIGQUIT, SIGWINCH};
use signal_hook::iterator::Signals;

use crate::app::{App, Effect, mint, session_command};
use crate::catalogue;
use crate::home::Launch;
use crate::keys::{Button, Event, MouseKind, Parser, Reply};
use crate::link::{self, Line};
use crate::look::Look;
use crate::mouse::Pointer;
use crate::retry::Retry;
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
    // The model lists are read off the loop: taken out before home
    // keeps the launch description.
    let models = std::mem::take(&mut launch.models);
    app.set_configure(launch.configure.take());
    // With no model configured the picker opens on home, before the
    // first frame: saving the choice writes the config default. It stays
    // shut when attaching to a session.
    let pick_at_start = launch.model.is_none() && launch.open_at == crate::OpenAt::Home;
    // The viewer's copies live under Fiber home, taken before home
    // keeps the launch description.
    let images_dir = launch.images.clone();
    app.set_home(launch);
    if pick_at_start {
        app.open_model_picker(crate::model_picker::Mode::Choose);
    }
    app.set_size(width, height);
    let retry = Retry::new(&clock);
    let open_command = crate::opener::command(|name| std::env::var_os(name), clipboard::on_path)
        .map(|argv| argv.into_iter().map(str::to_owned).collect());
    // The viewer opens what an opener opens with: none over SSH or
    // with no opener on PATH, where a click says why.
    let viewer = open_command
        .as_ref()
        .map(|_| crate::viewer::command(cfg!(target_os = "macos")))
        .unwrap_or_default();
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
        model_reader: catalogue::Reader::new(models),
        pointer: Pointer::default(),
        hover,
        var: Box::new(|name| std::env::var(name).ok()),
        copy_command: clipboard::command(|name| std::env::var_os(name), clipboard::on_path)
            .map(|argv| argv.into_iter().map(str::to_owned).collect()),
        open_command,
        viewer,
        images_dir,
        paste_reader: crate::paste_image::command(
            cfg!(target_os = "macos"),
            |name| std::env::var_os(name),
            clipboard::on_path,
        ),
        title: osc::Title::default(),
        shape: osc::Shape::default(),
        retry: Some(Arc::clone(&retry)),
        tick: crate::tick::TickThread::idle(),
    };
    terminal.app.set_opener(terminal.open_command.is_some());
    terminal
        .app
        .set_osc9(crate::attention::supported(&terminal.var));
    // Whether stripes draw is read once, before the first frame.
    crate::surface::init(&|name| std::env::var(name).ok());
    // The first frame's time: elapsed times and animation agree from it.
    terminal.app.set_now(
        terminal.clock.now(),
        contract::clock::wall_ms(terminal.clock.wall()),
    );
    // The first frame waits on nothing: the queries are out, and nothing
    // reads the tty or the hub until it is drawn.
    if terminal.screen.draw(&mut terminal.app, None).is_err() {
        return 1;
    }
    terminal.write_title();
    let (tx, rx) = mpsc::channel();
    terminal.files_out = Some(tx.clone());
    // The cached lists are asked after the first frame, so the first
    // frame draws at once and the picker lists them as soon as the read
    // answers.
    terminal.model_reader.ask(catalogue::Refresh::Cached, &tx);
    terminal.reader = terminal
        .tty
        .as_ref()
        .and_then(|tty| Reader::spawn(tty, tx.clone()));
    spawn_hub(connect, tx.clone(), retry, Arc::clone(&terminal.clock));
    // The tick runs only while something drawn moves: the first frame's
    // asks arm it, and the loop re-arms it after every frame.
    terminal.tick.start(Arc::clone(&terminal.clock), tx.clone());
    terminal.tick.arm(terminal.app.take_wake());
    if let Some(signals) = signals {
        spawn_resize(signals, tx);
    }
    let code = terminal.run(&rx);
    terminal.tick.stop();
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
    /// Where workers post their results.
    files_out: Option<Sender<Input>>,
    /// The `@` panel's search worker, while the panel is open.
    search: Option<files::Search>,
    /// The clipboard image command for this machine, chosen once at
    /// startup, or none where no clipboard reads.
    paste_reader: Option<crate::paste_image::Reader>,
    /// Inputs that arrived while a frame waited for history, handled next
    /// in arrival order.
    stash: VecDeque<Input>,
    /// The tty's reader, paused while the editor has the terminal.
    reader: Option<Reader>,
    /// The one model-list read at a time.
    model_reader: catalogue::Reader,
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
    /// The program a viewer worker runs with the copy appended, or none
    /// where nothing opens (`docs/tui.md`, "Images").
    viewer: Vec<String>,
    /// Where the viewer writes its copies: `cache/images` in Fiber home
    /// (`docs/state.md`).
    images_dir: PathBuf,
    /// The window title last written.
    title: osc::Title,
    /// The pointer shape last written.
    shape: osc::Shape,
    /// The hub thread's permit to connect again (`docs/tui.md`, "A
    /// dropped connection"); none in tests with no hub thread.
    retry: Option<Arc<Retry>>,
    /// The working line's timer: armed after every frame with the next
    /// moving frame's deadline (`docs/tui.md`, "The working line").
    tick: crate::tick::TickThread,
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
        self.app.set_now(
            self.clock.now(),
            contract::clock::wall_ms(self.clock.wall()),
        );
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
                            self.app.on_drag(&mouse);
                            self.app.on_wheel(&mouse);
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
                        Event::Reply(Reply::Appearance(appearance)) => {
                            self.screen.appearance(appearance);
                            Effect::None
                        }
                        Event::Reply(Reply::DeviceAttributes) => Effect::None,
                    };
                    match effect {
                        Effect::None => {}
                        Effect::Copy(text) => {
                            clipboard::copy(self.tty.as_ref(), self.copy_command.as_deref(), text);
                        }
                        Effect::OpenLink(url) => self.open_link(url),
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
                        Effect::ReadImage(ticket) => {
                            if let Some(notice) = crate::paste_image::start(
                                self.paste_reader.as_ref(),
                                &self.clock,
                                self.files_out.as_ref(),
                                ticket,
                            ) {
                                // Nothing started, so the running ticket's
                                // result can never arrive: landing the
                                // failure shows the notice and clears the
                                // gate for the next press.
                                self.app.on_image(ticket, Err(notice));
                            }
                        }
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
                self.retry_later();
            }
            Input::Disconnected => self.on_disconnected(),
            Input::Files { generation, result } => self.app.on_files(generation, result),
            Input::Models(result) => {
                self.app.on_models(result);
                // The queued read, if one waits, starts on the loop's
                // channel; without one there is no loop to answer.
                if let Some(out) = self.files_out.clone() {
                    self.model_reader.done(&out);
                }
            }
            Input::Image { ticket, result } => self.app.on_image(ticket, result),
            Input::Viewed {
                id,
                name,
                generation,
                result,
            } => self.app.image_viewed(id, &name, generation, result),
            Input::FindDue(generation) => {
                let lines = self.app.find_due(generation);
                self.send(&lines);
            }
            // Its frame already drew: the next moving frame arms the
            // tick again below.
            Input::Tick => self.tick.ack(),
            Input::Login { ticket, step } => {
                // Only the waiting ticket's `Open` opens anything: any
                // other ticket changes nothing and opens nothing.
                if let Some(url) = self.app.on_login(ticket, step) {
                    self.open_link(url);
                }
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
        // The model-list read the picker owes, if one is owed.
        if let (Some(refresh), Some(out)) = (self.app.take_reads(), &self.files_out) {
            self.model_reader.ask(refresh, &out.clone());
        }
        // The browser login the input asked for, if one waits: its
        // progress and its end arrive as `Input::Login`.
        if let Some(out) = self.files_out.clone()
            && let Some(start) = self.app.take_login_start()
        {
            let worker = crate::login_worker::start(start, Arc::clone(&self.clock), out);
            self.app.login_started(worker);
        }
        self.save_shares();
        // The reconciler's subscribes go out before the frame pages: an
        // acknowledgement arriving late is reconciled on the step that
        // brings it.
        let due = self.app.items_due(self.clock.now());
        self.send(&due);
        self.page_in(rx);
        self.apply_theme();
        // A selection's copy waiting on dropped pages runs once they load.
        if let Some(text) = self.app.take_copy() {
            clipboard::copy(self.tty.as_ref(), self.copy_command.as_deref(), text);
        }
        // The viewer opens the loop queued: one worker per view, each
        // answering once with `Input::Viewed`.
        self.drain_images();
        if self.screen.draw(&mut self.app, self.pointer.at).is_err() {
            return Some(1);
        }
        // What the frame asked wakes the tick: nothing moving arms
        // nothing.
        self.tick.arm(self.app.take_wake());
        self.write_title();
        self.write_shape();
        self.write_alerts();
        None
    }

    /// Opens the queued viewer copies: one worker per view, each
    /// answering once with `Input::Viewed`. Nothing is queued while
    /// the loop has no channel to answer on.
    fn drain_images(&mut self) {
        let Some(out) = self.files_out.clone() else {
            return;
        };
        for view in self.app.take_image_out().view {
            crate::viewer::spawn(&self.viewer, &self.images_dir, view, &out);
        }
    }

    /// Saves the shares a drag's release queued, in order, through the
    /// configuration seam.
    fn save_shares(&mut self) {
        for (key, share) in self.app.take_saves() {
            self.app.save_share(key, share);
        }
    }

    /// Writes the app's window title when it changed since the last write.
    fn write_title(&mut self) {
        if let Some(bytes) = self.title.next(self.app.title())
            && let Some(mut tty) = self.tty.as_ref()
        {
            tty.write_all(&bytes).unwrap_or(());
        }
    }

    /// Writes the pointer shape when it changed: the resize arrow over an
    /// edge or while a drag runs, else the default. Nothing with hover
    /// off, where the terminal never reports motion.
    fn write_shape(&mut self) {
        if !self.hover {
            return;
        }
        if let Some(bytes) = self.shape.next(self.app.over_edge(self.pointer.at))
            && let Some(mut tty) = self.tty.as_ref()
        {
            tty.write_all(bytes).unwrap_or(());
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
            let Some(session) = self.app.paging_session().cloned() else {
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
                Input::Hub(Line::Session(line)) if link::answers(&line, id) => {
                    return link::history_answer(&line);
                }
                // A tick while a frame waits for history is acked and
                // dropped, not stashed: the frame asks again after it
                // draws.
                Input::Tick => self.tick.ack(),
                Input::Disconnected => {
                    self.on_disconnected();
                    return Err(LOST.to_owned());
                }
                other @ (Input::Bytes(_)
                | Input::Hub(_)
                | Input::Connected(..)
                | Input::ConnectFailed(_)
                | Input::Resize
                | Input::FindDue(_)
                | Input::Image { .. }
                | Input::Viewed { .. }
                | Input::Models(_)
                | Input::Login { .. }
                | Input::Files { .. }) => self.stash.push_back(other),
            }
        }
    }

    /// The hub connection ended: the reader sends this once per stream,
    /// so each ended connection gives the hub thread one permit.
    fn on_disconnected(&mut self) {
        self.hub = None;
        self.app.disconnected();
        self.retry_later();
    }

    /// Gives the hub thread its permit for one failure, after the delay
    /// the app names; none once the hub refused this terminal's schema.
    fn retry_later(&mut self) {
        if let (Some(delay), Some(retry)) = (self.app.next_retry(), &self.retry) {
            retry.give(delay);
        }
    }

    /// The connection ended during a fetch: nothing more is asked. The
    /// reader's end of the stream gives the permit.
    fn lost(&mut self) {
        self.hang_up();
        self.app.disconnected();
    }

    /// Writes command lines to the hub, each one written kept for resending
    /// until it is answered. A failed write hangs up: the connection is
    /// lost, and the text of the lines not sent returns to the draft.
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
            self.app.wrote(line);
        }
    }

    /// Opens `url` with the terminal's opener: `open` or `xdg-open` on the
    /// terminal's machine. Over SSH or with no opener nothing launches;
    /// the URL is drawn either way and the flow completes when it is
    /// visited (`docs/tui.md`, "Links").
    fn open_link(&self, url: String) {
        if let Some(argv) = self.open_command.clone() {
            let mut argv = argv;
            argv.push(url);
            drop(clipboard::pipe(argv, String::new()));
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
        // The restore put the default pointer back: the next frame writes
        // the shape again.
        self.shape.reset();
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

/// The loop is gone: the hub thread stops waiting for a permit, and a
/// reader blocked on the stream sees its end.
impl<B: Backend> Drop for Loop<B> {
    fn drop(&mut self) {
        if let Some(retry) = &self.retry {
            retry.quit();
        }
        self.hang_up();
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lib_login_tests.rs"]
mod login_tests;

#[cfg(test)]
#[path = "lib_image_tests.rs"]
mod image_tests;

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
#[path = "lib_delegates_tests.rs"]
mod delegates_tests;
#[cfg(test)]
#[path = "lib_panel_tests.rs"]
mod panel_tests;
#[cfg(test)]
#[path = "lib_rail_tests.rs"]
mod rail_tests;

#[cfg(test)]
#[path = "lib_drag_tests.rs"]
mod drag_tests;

#[cfg(test)]
#[path = "lib_attention_tests.rs"]
mod attention_tests;

#[cfg(test)]
#[path = "lib_look_tests.rs"]
mod look_tests;

#[cfg(test)]
#[path = "lib_settings_tests.rs"]
mod settings_tests;

#[cfg(test)]
#[path = "lib_paste_tests.rs"]
mod paste_tests;

#[cfg(test)]
#[path = "lib_reconnect_tests.rs"]
mod reconnect_tests;
