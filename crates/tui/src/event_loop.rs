//! The event loop (`docs/tui.md`, "History and paging"): `run` sets the
//! terminal up, draws the first frame and starts the input threads; the loop
//! handles one batch of ready inputs at a time, fetches the history pages a frame needs
//! before drawing it, and hands the terminal to the editor.

pub(crate) mod batch;
mod hub;
#[cfg(test)]
mod pause;

use batch::batchable;
use hub::Connection;

use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
#[cfg(not(test))]
use std::time::Instant;

use contract::clock::Clock;
use contract::{ErrorCode, SessionId};
use ratatui::backend::Backend;
use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGWINCH};
use signal_hook::iterator::Signals;

use crate::app::{App, Effect};
use crate::catalogue;
use crate::home::Launch;
use crate::keys::{Event, MouseKind, Parser, Reply};
use crate::link::{Feed, Line};
use crate::look::Look;
use crate::mouse::Pointer;
use crate::retry::Retry;
use crate::screen::Screen;
use crate::sources::{Reader, spawn_hub, spawn_resize};
use crate::{Connect, Input, OnAttach, clipboard, editor, files, osc, term};

/// What keeps the terminal from running.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The input reader could not start, so no key would reach the loop.
    #[error(transparent)]
    ReaderStart(#[from] crate::sources::SpawnError),
    /// The working line's timer could not start, so nothing moving on
    /// screen would move.
    #[error("terminal: could not start the terminal's timer: {0}")]
    TimerStart(#[from] std::io::Error),
}

impl Error {
    /// The stable code a consumer switches on (`docs/errors.md`, "Registry").
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::ReaderStart(_) | Self::TimerStart(_) => ErrorCode::IoFailed,
        }
    }
}

/// Runs the terminal on `tty` for `launch`, starting sessions in its
/// workspace. `hover` is `launch.hover`: with it off, mouse mode 1003 is
/// never sent and nothing is tinted under the pointer. `Ok(0)` on quit
/// and `Ok(1)` when the terminal cannot be set up or drawn. The terminal
/// is restored on every return. Timed scopes write through `diag`, the
/// process's diagnostic writer.
pub fn run(
    tty: File,
    launch: Launch,
    connect: Connect,
    on_attach: OnAttach,
    clock: Arc<dyn Clock>,
    diag: Arc<log::diag::Diag>,
) -> Result<i32, Error> {
    run_with(
        tty,
        launch,
        connect,
        on_attach,
        clock,
        Reader::spawn,
        |builder, body| builder.spawn(body),
        diag,
    )
}

/// Runs as [`run`] does, with the input reader's spawn and the working
/// line's timer spawn as functions so tests fail them. A failed spawn
/// returns the failure without entering the loop: the terminal is
/// restored by the [`term::Guard`] drop on that return.
#[allow(
    clippy::too_many_arguments,
    reason = "each seam fails on its own in tests"
)]
pub(crate) fn run_with(
    tty: File,
    launch: Launch,
    connect: Connect,
    on_attach: OnAttach,
    clock: Arc<dyn Clock>,
    spawn_reader: impl FnOnce(&File, Sender<Input>) -> Result<Reader, crate::sources::SpawnError>,
    spawn_tick: impl FnOnce(
        std::thread::Builder,
        Box<dyn FnOnce() + Send + 'static>,
    ) -> std::io::Result<std::thread::JoinHandle<()>>,
    diag: Arc<log::diag::Diag>,
) -> Result<i32, Error> {
    // The first render's span opens at entry: at `info` this is one level
    // check with no clock read. Every pre-draw return below discards it,
    // so a line there never times setup work as a frame that never drew.
    let timing = crate::timing::Timing::new(Arc::clone(&diag));
    let first_render = timing.first_render();
    // SIGWINCH is caught from before the size is read, so no resize is
    // missed; its thread starts after the first frame.
    let signals = Signals::new([SIGWINCH]).ok();
    // SIGTERM and SIGHUP take the normal exit path below: the loop quits
    // with 128 plus the signal's number after restoring the terminal and
    // printing the resume lines. Without the registration a signal would
    // kill the process without restoring, so a refused one fails before
    // any frame.
    let Ok(term_signals) = Signals::new([SIGTERM, SIGHUP]) else {
        crate::timing::discard(first_render);
        return Ok(1);
    };
    catch_interrupts();
    let restore = term::Guard;
    // The first frame waits for the opening pass and local TUI loading,
    // never a child process or a tty/hub read.
    let hover = launch.hover;
    let Ok((width, height)) = term::setup(&tty, hover) else {
        crate::timing::discard(first_render);
        return Ok(1);
    };
    let Ok(out) = tty.try_clone() else {
        crate::timing::discard(first_render);
        return Ok(1);
    };
    let Ok(mut screen) = Screen::new(crate::screen::backend(out), width, height) else {
        crate::timing::discard(first_render);
        return Ok(1);
    };
    // `App::new` maps an empty launch directory to no workspace, so a
    // remote home carries none.
    let mut app = App::new(
        launch
            .location
            .workspace()
            .map(std::path::Path::to_path_buf),
    );
    app.set_zone(jiff::tz::TimeZone::system());
    let mut launch = launch;
    let (look, notice) = Look::new(std::mem::take(&mut launch.theme), &|name| {
        std::env::var(name).ok()
    });
    screen.set_look(look);
    if let Some(notice) = notice {
        app.push_notice("config_invalid", notice);
    }
    app.set_keys(std::mem::take(&mut launch.keys));
    // The model lists are read off the loop: taken out before home
    // keeps the launch description.
    let models = std::mem::take(&mut launch.models);
    let router = crate::remote::Router::new();
    let seams = crate::remote::seams(&launch.location, launch.configure.take(), models, &router);
    let extensions = launch.extensions.clone();
    let models = seams.models;
    if let Some(remote) = seams.remote {
        app.set_remote(remote);
    } else {
        app.set_configure(seams.seam);
    }
    app.set_models(models.clone());
    // The feedback writer `main` injected: taken out before home keeps
    // the launch description, as the model lists are.
    app.set_feedback(std::mem::take(&mut launch.feedback));
    // With no model configured the picker opens on home, before the
    // first frame: saving the choice writes the config default. It stays
    // shut when attaching to a session.
    let pick_at_start = launch.model.is_none() && launch.open_at == crate::OpenAt::Home;
    // The viewer's copies live under Fiber home, taken before home
    // keeps the launch description.
    let images_dir = launch.images.clone();
    // Bare `fiber`'s `--model` and `-c`, normalized into the per-run
    // layer: taken out before home keeps the launch description, so
    // each `start` carries them in `overrides`.
    let launch_overrides = std::mem::take(&mut launch.overrides);
    app.set_home(launch);
    // The first-run import offer resolves once, through the same seam
    // the views read: no receipts means the home foot asks.
    let pending = app
        .configure_seam()
        .is_some_and(|seam| seam.import_pending());
    app.set_import_offer(pending);
    app.set_launch_overrides(launch_overrides);
    if pick_at_start {
        app.start_model_pick();
    }
    // One cell's pixels, for the placeholder grid, from the tty the
    // first frame already sized (`docs/tui.md`, "Images").
    app.set_cell(term::cell(term::pixels(&tty), (width, height)));
    app.set_size(width, height);
    let retry = Retry::new(&clock);
    let open = crate::opener::command(|name| std::env::var_os(name), clipboard::on_path)
        .map(|argv| argv.into_iter().map(str::to_owned).collect());
    // The viewer opens what an opener opens with: none over SSH or
    // with no opener on PATH, where a click says why.
    let viewer = open
        .as_ref()
        .map(|_| crate::viewer::command(cfg!(target_os = "macos")))
        .unwrap_or_default();
    let programs = Programs {
        copy: clipboard::command(|name| std::env::var_os(name), clipboard::on_path)
            .map(|argv| argv.into_iter().map(str::to_owned).collect()),
        open,
        viewer,
        paste: crate::paste_image::command(
            cfg!(target_os = "macos"),
            |name| std::env::var_os(name),
            clipboard::on_path,
        ),
        images_dir,
    };
    app.set_opener(programs.open.is_some());
    let var: Var = Box::new(|name| std::env::var(name).ok());
    app.set_osc9(crate::attention::supported(&var));
    // Whether stripes draw is read once, before the first frame.
    crate::surface::init(&|name| std::env::var(name).ok());
    // The first frame's time: elapsed times and animation agree from it.
    app.set_now(clock.now(), contract::clock::wall_ms(clock.wall()));
    // Loading is synchronous: read, compile and initialization share one
    // deadline started before reading. Uninterruptible overruns fail on return.
    // The queries are out; no tty or hub read precedes the first frame.
    if let Some(loader) = extensions {
        let (renderer, notices) = loader.load();
        app.use_renderer(renderer);
        for (code, text) in notices {
            app.push_notice(&code, text);
        }
    }
    if screen.draw(&mut app, None).is_err() {
        crate::timing::discard(first_render);
        return Ok(1);
    }
    // The frame drew: its line stands, whatever the loop meets after.
    drop(first_render);
    // Startup pushes (config reads, session restores) gain births and
    // wakes before the first arm, so they fade like every other event.
    app.sweep_notices(clock.now());
    // The title goes out before the reader starts, so a failed start
    // still leaves it set.
    let mut title = osc::Title::default();
    if let Some(bytes) = title.next(app.title()) {
        let mut out = &tty;
        out.write_all(&bytes).unwrap_or(());
    }
    let (tx, rx) = mpsc::channel();
    // The cached lists are asked after the first frame, so the first
    // frame draws at once and the picker lists them as soon as the read
    // answers. The ask bypasses the owed picker reads, so it counts as
    // owed, or its answer would clear a meanwhile taken read's mark.
    app.note_catalogue_read();
    let mut model_reader = catalogue::Reader::new(models);
    model_reader.ask(catalogue::Refresh::Cached, &tx);
    // Without the reader no key reaches the loop: fail instead of
    // running deaf. The guard's drop restores the terminal.
    let reader = spawn_reader(&tty, tx.clone())?;
    let mut terminal = Loop {
        app,
        parser: Parser::default(),
        screen,
        hub: Connection::new(Arc::clone(&retry)),
        tty,
        on_attach,
        clock,
        timing,
        #[cfg(not(test))]
        trace_diag: Arc::clone(&diag),
        #[cfg(not(test))]
        trace_hub_connect: None,
        #[cfg(not(test))]
        trace_tail_frame: None,
        files_out: tx.clone(),
        search: None,
        reader,
        model_reader,
        pointer: Pointer::default(),
        hover,
        var,
        programs,
        title,
        shape: osc::Shape::default(),
        tick: crate::tick::TickThread::idle(),
    };
    #[cfg(not(test))]
    {
        terminal.trace_hub_connect = log::diag::timed(
            &terminal.trace_diag,
            contract::diag::Op::TuiLogRead,
            Some("attach.hub_connect_proxy"),
            Some(terminal.clock.now()),
        );
    }
    spawn_hub(
        connect,
        Feed::new(tx.clone(), terminal.hub.credits()).routed(Arc::clone(&router)),
        retry,
        Arc::clone(&terminal.clock),
    );
    // The tick runs only while something drawn moves: the first frame's
    // asks arm it, and the loop re-arms it after every frame. Without
    // the tick nothing moving on screen moves: fail instead of running
    // still.
    terminal
        .tick
        .start_with(Arc::clone(&terminal.clock), tx.clone(), spawn_tick)?;
    terminal.tick.arm(terminal.app.take_wake());
    spawn_signal(term_signals, tx.clone());
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
    terminal.hub.hang_up();
    drop(restore);
    let mut tty = &terminal.tty;
    for line in &lines {
        writeln!(tty, "{line}").unwrap_or(());
    }
    Ok(code)
}

/// Turns SIGTERM and SIGHUP into [`Input::Signal`] on their own thread,
/// in the iterator's order, as the session's signal handling takes it
/// (`docs/invocation.md`, "Three signals, one path"): co-pending
/// signals have no bound order, and no test pins one. Left blocked on
/// quit; it ends with the process.
fn spawn_signal(mut signals: Signals, tx: Sender<Input>) {
    let signal = crate::sources::builder("tui-signal").spawn(move || {
        for signal in signals.forever() {
            if tx.send(Input::Signal(signal)).is_err() {
                return;
            }
        }
    });
    drop(signal);
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

/// The programs the loop runs for the person, each chosen once at startup.
#[derive(Default)]
struct Programs {
    /// The system clipboard command a copy is piped to, beside OSC 52.
    copy: Option<Vec<String>>,
    /// The program a link click runs with the URL appended, or none over
    /// SSH or with no opener on `PATH` (`docs/tui.md`, "Links").
    open: Option<Vec<String>>,
    /// The program a viewer worker runs with the copy appended, or none
    /// where nothing opens (`docs/tui.md`, "Images").
    viewer: Vec<String>,
    /// The clipboard image command for this machine, or none where no
    /// clipboard reads.
    paste: Option<crate::paste_image::Reader>,
    /// Where the viewer writes its copies: `cache/images` in Fiber home
    /// (`docs/state.md`).
    images_dir: PathBuf,
}

/// The loop's state.
struct Loop<B: Backend> {
    app: App,
    parser: Parser,
    screen: Screen<B>,
    /// The hub connection: the stream, the held inputs and the hub
    /// thread's permit, reached only through its interface.
    hub: Connection,
    /// The tty, for its size on resize.
    tty: File,
    on_attach: OnAttach,
    clock: Arc<dyn Clock>,
    /// The diagnostic writer the timed scopes share.
    timing: crate::timing::Timing,
    /// Temporary attach-trace writer, removed after #3501's trace run.
    #[cfg(not(test))]
    trace_diag: Arc<log::diag::Diag>,
    /// The hub-connect proxy started when the hub reader is spawned.
    #[cfg(not(test))]
    trace_hub_connect: Option<log::diag::Timer>,
    /// The first tail frame's start, after the preview installs.
    #[cfg(not(test))]
    trace_tail_frame: Option<Instant>,
    /// Where workers post their results.
    files_out: Sender<Input>,
    /// The `@` panel's search worker, while the panel is open.
    search: Option<files::Search>,
    /// The tty's reader, paused while the editor has the terminal.
    reader: Reader,
    /// The one model-list read at a time.
    model_reader: catalogue::Reader,
    /// The pointer's last cell and a pending click.
    pointer: Pointer,
    /// `tui.hover`: whether the pointer's cell is recorded and tinted.
    hover: bool,
    /// Reads an environment variable: `$VISUAL` and `$EDITOR` for Ctrl+G.
    var: Var,
    /// The programs this machine runs for the person.
    programs: Programs,
    /// The window title last written.
    title: osc::Title,
    /// The pointer shape last written.
    shape: osc::Shape,
    /// The working line's timer: armed after every frame with the next
    /// moving frame's deadline (`docs/tui.md`, "The working line").
    tick: crate::tick::TickThread,
}

impl<B: Backend> Loop<B> {
    /// Handles inputs until one quits, or every sender is gone: those a
    /// frame held while it waited first, then the channel's. Ready hub
    /// lines and ticks share one frame; anything else ends the batch
    /// for its own. The only wait is `recv` with no timeout.
    fn run(&mut self, rx: &Receiver<Input>) -> i32 {
        loop {
            // Nothing stashed and every sender gone ends the loop.
            let Some(first) = self.hub.take_batch(rx) else {
                return 0;
            };
            // Only a batch starting with a hub line or tick holds: a
            // key, click or resize folds nothing into a held page, and
            // opening a delegate swaps the shown screen, which must
            // never happen mid-hold. A hub batch folds before it counts:
            // the open page is counted once for the batch, not once per
            // changed line, and the batch settles once at its end, when
            // the counts are exact.
            let held = batchable(first.first());
            // The batch's inputs already folded, in arrival order.
            if held {
                self.app.begin_batch();
            }
            let mut code = None;
            // A frame separates two Bytes inputs unless both are
            // wheel-only, so every other input keeps the frame it gets
            // today. `framed` avoids a pre-frame before the first input;
            // non-Bytes batches never enter this branch.
            let mut wheel_prev = false;
            let mut framed = false;
            // A hub batch folds chunk by chunk inside one logical batch:
            // one hold and one frame for the batch, so a stream that
            // never empties still draws, and a key waits only for the
            // hub inputs that arrived before it.
            let mut taken: usize = 0;
            let mut chunk = Some(first);
            while let Some(batch) = chunk.take() {
                #[cfg(test)]
                if held {
                    pause::before_fold();
                }
                for input in batch {
                    taken = taken.saturating_add(1);
                    if let Input::Bytes(bytes) = input {
                        let events = self.parser.feed(&bytes);
                        let wheel = Self::wheel_only(&events);
                        if framed
                            && !(wheel_prev && wheel)
                            && let Some(quit) = self.frame(rx)
                        {
                            code = Some(quit);
                            break;
                        }
                        framed = true;
                        wheel_prev = wheel;
                        if let Some(quit) = self.handle_events(events) {
                            code = Some(quit);
                            break;
                        }
                    } else if let Some(quit) = self.handle(input) {
                        code = Some(quit);
                        break;
                    }
                }
                if code.is_some() {
                    break;
                }
                chunk = if held {
                    self.hub.next_chunk(rx, taken)
                } else {
                    None
                };
            }
            // The batch's end runs on every path out, so a quit never
            // leaves the hold set.
            if held {
                self.app.end_batch();
                #[cfg(test)]
                pause::batch_ended(taken);
            }
            if let Some(code) = code {
                return code;
            }
            if let Some(code) = self.frame(rx) {
                return code;
            }
        }
    }

    /// Whether `events` hold only wheel steps: a non-empty run of wheel
    /// reports, so a batch of them shares one frame.
    fn wheel_only(events: &[Event]) -> bool {
        !events.is_empty()
            && events.iter().all(|event| {
                matches!(
                    event,
                    Event::Mouse(mouse)
                        if matches!(mouse.kind, MouseKind::WheelUp | MouseKind::WheelDown)
                )
            })
    }

    /// Handles parsed terminal events: the time, what each does to the
    /// app, and what each sends. `run` parses each read first, so a Bytes
    /// batch can share a frame across a run of wheel steps; `handle`
    /// parses reads itself. Returns the exit code when the terminal
    /// quits, before any frame.
    fn handle_events(&mut self, events: Vec<Event>) -> Option<i32> {
        self.app.set_now(
            self.clock.now(),
            contract::clock::wall_ms(self.clock.wall()),
        );
        let before = self.app.session().cloned();
        for event in events {
            let quota = if let Event::Focus(true) = event {
                self.app.quota_focus()
            } else if matches!(event, Event::Stroke(_) | Event::Key(_) | Event::Edit(_)) {
                self.app.quota_keypress()
            } else {
                None
            };
            if let Some(line) = quota {
                self.hub.send(&[line], &mut self.app);
            }
            let effect = match event {
                Event::Stroke(stroke) => self.app.on_press(stroke, self.clock.now()),
                Event::Key(key) => self.app.on_key(key, self.clock.now()),
                Event::Edit(edit) => self.app.on_edit(edit),
                Event::Mouse(mouse) => self.app.on_mouse(
                    &mouse,
                    self.screen.targets(),
                    &mut self.pointer,
                    self.hover,
                    self.clock.now(),
                ),
                Event::Reply(Reply::KittyFlags(_)) => {
                    self.kitty();
                    Effect::None
                }
                Event::Reply(Reply::Appearance(appearance)) => {
                    self.screen.appearance(appearance);
                    Effect::None
                }
                Event::Reply(Reply::Graphics { id, ok }) => {
                    self.graphics(id, ok);
                    Effect::None
                }
                Event::Reply(Reply::DeviceAttributes) | Event::Focus(_) => Effect::None,
            };
            match effect {
                Effect::None => {}
                Effect::Copy(text) => {
                    clipboard::copy(
                        Some(&self.tty),
                        self.programs.copy.as_deref(),
                        text,
                        &self.clock,
                    );
                }
                Effect::OpenLink(url) => self.open_link(url),
                Effect::Send(lines) => self.hub.send(&lines, &mut self.app),
                Effect::Quit => return Some(0),
                Effect::Exit(lines) => {
                    // The quit question's closes go out, then the
                    // terminal quits: a close never written keeps
                    // its resume line.
                    self.hub.send(&lines, &mut self.app);
                    return Some(0);
                }
                Effect::ListFiles => self.list_files(),
                Effect::ReadImage(ticket) => {
                    if let Some(notice) = crate::paste_image::start(
                        self.programs.paste.as_ref(),
                        &self.clock,
                        Some(&self.files_out),
                        ticket,
                    ) {
                        // Nothing started, so the running ticket's
                        // result can never arrive: landing the
                        // failure shows the notice and clears the
                        // gate for the next press.
                        self.app.on_image(ticket, Err(notice));
                    }
                }
                Effect::Feedback { description } => {
                    if let Some(notice) = crate::feedback::start(
                        self.app.feedback_writer(),
                        Some(&self.files_out),
                        description,
                        self.app.session().cloned(),
                    ) {
                        // Nothing started, so no answer can arrive:
                        // landing the failure shows the notice.
                        self.app.on_feedback(Err(notice));
                    }
                }
                Effect::FindPause { generation, after } => {
                    let clock = Arc::clone(&self.clock);
                    let out = self.files_out.clone();
                    // A pause thread outliving the loop finds
                    // the channel closed and returns.
                    drop(crate::sources::builder("tui-find-pause").spawn(move || {
                        clock.sleep(after);
                        drop(out.send(Input::FindDue(generation)));
                    }));
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
        self.after_input(before)
    }

    /// The bookkeeping every handled input ends with: reporting an attach
    /// the input brought, dropping a closed panel worker, asking a read
    /// the picker owes, and starting a login that waits.
    fn after_input(&mut self, before: Option<SessionId>) -> Option<i32> {
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
        if let Some(refresh) = self.app.take_reads() {
            self.model_reader.ask(refresh, &self.files_out);
        }
        // The browser login the input asked for, if one waits: its
        // progress and its end arrive as `Input::Login`.
        if let Some(start) = self.app.take_login_start() {
            let worker =
                crate::login_worker::start(start, Arc::clone(&self.clock), self.files_out.clone());
            self.app.login_started(worker);
        }
        // The install the input chose, if one waits: its end arrives as
        // `Input::Install`.
        if let Some(start) = self.app.take_install_start() {
            crate::install_worker::start(start, Arc::clone(&self.clock), self.files_out.clone());
        }
        // The hooks offer's query or install, if one waits: its end
        // arrives as `Input::HooksOfferNames` or `Input::HooksInstalled`.
        if let Some(job) = self.app.take_hooks_job() {
            crate::install_worker::start_hooks(
                job,
                Arc::clone(&self.clock),
                self.files_out.clone(),
            );
        }
        self.save_shares();
        None
    }

    /// Handles one input: the time, what it does to the app, and what it
    /// sends. Returns the exit code when the terminal quits, before any
    /// frame, as handling every input in `step` did.
    fn handle(&mut self, input: Input) -> Option<i32> {
        self.app.set_now(
            self.clock.now(),
            contract::clock::wall_ms(self.clock.wall()),
        );
        let before = self.app.session().cloned();
        match input {
            // A signal quits with 128 plus its number, closing nothing
            // first: live sessions keep their resume lines.
            Input::Signal(signal) => return Some(128 + signal),
            Input::Bytes(bytes) => {
                let events = self.parser.feed(&bytes);
                return self.handle_events(events);
            }
            Input::Hub(line) => {
                let lines = self.app.on_line(line);
                self.hub.send(&lines, &mut self.app);
            }
            Input::Connected(stream, hello) => {
                #[cfg(not(test))]
                drop(self.trace_hub_connect.take());
                self.hub.store(stream);
                // A stream whose write timeout would not set is shut
                // down, not adopted: the connection failed, as a refused
                // connect does. The reader's end of it retries like any
                // other lost connection.
                if !self.hub.has_stream() {
                    self.app.connect_failed(
                        "Could not set up the hub connection: its write timeout could not be set"
                            .to_owned(),
                    );
                } else {
                    let lines = self.app.on_line(Line::Hub(hello));
                    self.hub.send(&lines, &mut self.app);
                    // A `hub_hello` this terminal cannot read leaves it
                    // unconnected: it says so and disconnects.
                    if !self.app.connected() {
                        self.hub.hang_up();
                    }
                }
            }
            Input::ConnectFailed(error) => {
                self.app
                    .connect_failed(format!("Could not reach the hub: {error}"));
                self.hub.retry_later(&mut self.app);
            }
            Input::Disconnected => self.hub.on_disconnected(&mut self.app),
            Input::Files { generation, result } => self.app.on_files(generation, result),
            Input::Models(result) => {
                self.app.on_models(result);
                // The queued read, if one waits, starts on the loop's
                // channel.
                self.model_reader.done(&self.files_out);
            }
            Input::Image { ticket, result } => self.app.on_image(ticket, result),
            Input::Feedback(result) => self.app.on_feedback(result),
            Input::Viewed {
                id,
                name,
                generation,
                result,
            } => self.app.image_viewed(id, &name, generation, result),
            Input::FindDue(generation) => {
                let lines = self.app.find_due(generation);
                self.hub.send(&lines, &mut self.app);
            }
            // Its frame already drew: the next moving frame arms the
            // tick again below.
            Input::Tick => self.tick.ack(),
            Input::Install { provider, result } => self.app.on_install(provider, result),
            Input::HooksOfferNames { ticket, result } => {
                self.app.on_hooks_offer_names(ticket, result)
            }
            Input::HooksInstalled { ticket, result } => {
                let lines = self.app.on_hooks_installed(ticket, result);
                self.hub.send(&lines, &mut self.app);
            }
            Input::Login { ticket, step } => {
                // Only the waiting ticket's `Open` opens anything: any
                // other ticket changes nothing and opens nothing.
                if let Some(url) = self.app.on_login(ticket, step) {
                    self.open_link(url);
                }
            }
            Input::Resize => {
                if let Ok((width, height)) = term::size(&self.tty) {
                    self.app
                        .set_cell(term::cell(term::pixels(&self.tty), (width, height)));
                    self.app.set_size(width, height);
                    if self.screen.resize(width, height).is_err() {
                        return Some(1);
                    }
                }
            }
        }
        self.after_input(before)
    }

    /// Loads the pages the frame needs, and draws what changed: once per
    /// batch, after every input in it is handled. Returns the exit code
    /// when the terminal cannot be drawn.
    fn frame(&mut self, rx: &Receiver<Input>) -> Option<i32> {
        // A frame past its 16 ms budget writes `tui.frame`; one within
        // budget writes no line, so scrolling leaves the log small.
        let _frame = self.timing.frame();
        // The reconciler's subscribes go out before the frame pages: an
        // acknowledgement arriving late is reconciled on the step that
        // brings it.
        let now = self.clock.now();
        let due = self.app.items_due(now);
        self.hub.send(&due, &mut self.app);
        // A rewind `history` wait past its bound ends as a lost
        // connection, exactly as the loop's own history wait does.
        if self.app.rewind_due(now) {
            self.hub.lost(&mut self.app);
        }
        // The open's tail reads before the frame pages: the first frame
        // draws the preview it installs.
        self.open_tail(rx);
        self.page_in(rx);
        self.apply_theme();
        // A selection's copy waiting on dropped pages runs once they load,
        // and a `/copy` waiting on an opening pass once the pass ends.
        if let Some(text) = self.app.take_copy() {
            clipboard::copy(
                Some(&self.tty),
                self.programs.copy.as_deref(),
                text,
                &self.clock,
            );
        }
        if let Some(text) = self.app.take_owed_copy() {
            clipboard::copy(
                Some(&self.tty),
                self.programs.copy.as_deref(),
                text,
                &self.clock,
            );
        }
        // A call view's `y` copy waiting on its pages runs once they
        // all land; a failed page aborts it with no write.
        if let Some(text) = self.app.take_call_copy() {
            clipboard::copy(
                Some(&self.tty),
                self.programs.copy.as_deref(),
                text,
                &self.clock,
            );
        }
        // The viewer opens the loop queued: one worker per view, each
        // answering once with `Input::Viewed`. The display commands and
        // kitty bytes wait for the post-draw drain: the draw queues the
        // frame's fetches.
        self.drain_images();
        // A session change queued deletes for its shown images: they
        // leave the terminal before the draw, so the next session never
        // draws under a stale id (`docs/tui.md`, "Images").
        self.drain_image_deletes();
        // The pre-draw sweep births in-frame pushes (page-in loads,
        // image failures) so the draw sees them; the post-draw sweep
        // births renderer-failure pushes before the arm, so a push
        // already past the fade expires before arming and never draws
        // twice.
        self.app.sweep_notices(self.clock.now());
        #[cfg(not(test))]
        let tail_frame = self.trace_tail_frame.take().map(|started| {
            log::diag::timed(
                &self.trace_diag,
                contract::diag::Op::TuiFirstRender,
                Some("attach.first_tail_frame"),
                Some(started),
            )
        });
        if self.screen.draw(&mut self.app, self.pointer.at).is_err() {
            return Some(1);
        }
        #[cfg(not(test))]
        drop(tail_frame);
        // The retained count reads after the first frame draws and
        // before the `full` raise goes out below, so no ancestor window
        // is asked before the own-segment screen shows.
        self.count_tail(rx);
        // The display fetches the frame drew go out after it, with the
        // kitty bytes their answers queued (`docs/tui.md`, "Images").
        self.drain_display();
        self.app.sweep_notices(self.clock.now());
        // The open's `full` raise goes out after the first frame draws,
        // so no `full` line is written before it.
        if let Some(line) = self.app.take_raise() {
            self.hub.send(&[line], &mut self.app);
        }
        // What the frame asked wakes the tick: nothing moving arms
        // nothing.
        self.tick.arm(self.app.take_wake());
        self.write_title();
        self.write_shape();
        self.write_alerts();
        None
    }

    /// Handles one input and draws the frame. Tests only: production
    /// code draws once per batch through `run`.
    #[cfg(test)]
    fn step(&mut self, input: Input, rx: &Receiver<Input>) -> Option<i32> {
        if let Input::Bytes(bytes) = input {
            let events = self.parser.feed(&bytes);
            if let Some(code) = self.handle_events(events) {
                return Some(code);
            }
        } else if let Some(code) = self.handle(input) {
            return Some(code);
        }
        self.frame(rx)
    }

    /// Opens the queued viewer copies: one worker per view, each
    /// answering once with `Input::Viewed`. The display commands and
    /// kitty bytes stay queued for the post-draw drain.
    fn drain_images(&mut self) {
        for view in self.app.take_image_views() {
            crate::viewer::spawn(
                &self.programs.viewer,
                &self.programs.images_dir,
                view,
                &self.files_out,
                &self.clock,
            );
        }
    }

    /// Writes the queued session-change deletes before the screen draw:
    /// a later session reuses image ids from 1, so its first frame never
    /// draws under a stale image (`docs/tui.md`, "Images").
    fn drain_image_deletes(&mut self) {
        let deletes = self.app.take_image_deletes();
        if !deletes.is_empty() {
            let mut tty = &self.tty;
            tty.write_all(&deletes).unwrap_or(());
        }
    }

    /// Sends the display `read_file` commands the frame drew, and writes
    /// the kitty transmit and place bytes their answers queued, after the
    /// screen draw: each posting reaches the terminal through one write
    /// (`docs/tui.md`, "Images").
    fn drain_display(&mut self) {
        let out = self.app.take_image_out();
        self.hub.send(&out.display, &mut self.app);
        if !out.kitty.is_empty() {
            let mut tty = &self.tty;
            tty.write_all(&out.kitty).unwrap_or(());
        }
    }

    /// Saves the settings a show, hide or release queued, in order,
    /// through the configuration seam.
    fn save_shares(&mut self) {
        for (key, value) in self.app.take_saves() {
            self.app.save_setting(key, &value);
        }
    }

    /// Writes the app's window title when it changed since the last write.
    fn write_title(&mut self) {
        if let Some(bytes) = self.title.next(self.app.title()) {
            let mut tty = &self.tty;
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
        if let Some(bytes) = self.shape.next(self.app.over_edge(self.pointer.at)) {
            let mut tty = &self.tty;
            tty.write_all(bytes).unwrap_or(());
        }
    }

    /// Writes the attention bytes this input queued (`docs/tui.md`,
    /// "Getting the person's attention"). A failed write is dropped.
    fn write_alerts(&mut self) {
        let bytes = self.app.take_alerts();
        let mut tty = &self.tty;
        tty.write_all(&bytes).unwrap_or(());
    }

    /// Kitty's graphics reply: the probe's success records graphics
    /// support, so later frames may draw images inline (`docs/tui.md`,
    /// "Images"). An error fails only its matching display image, with
    /// one notice: the line stays, and nothing asks again while its
    /// session's layout lives.
    fn graphics(&mut self, id: u32, ok: bool) {
        if id == term::PROBE_ID {
            // The probe's success records graphics support; its error,
            // like its absence, leaves the one-line form.
            if ok {
                self.app.set_graphics();
            }
            return;
        }
        // An error fails only its matching display image, with one
        // notice: the line stays, and nothing asks again while its
        // session's layout lives (`docs/tui.md`, "Images").
        if !ok {
            self.app.fail_display_image(id);
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
        let mut tty = &self.tty;
        if io::Write::write_all(&mut tty, term::KITTY_PUSH).is_ok() {
            self.parser.set_kitty();
        }
    }

    /// Starts the `@` panel's search worker on a listing of the workspace,
    /// searching for an empty query at the current generation.
    fn list_files(&mut self) {
        // With no local workspace there is no directory to list.
        let Some(workspace) = self.app.workspace() else {
            return;
        };
        let roots = files::Roots::new(workspace.clone(), std::env::var_os("HOME"));
        let search = files::Search::spawn(
            move || files::list(&workspace),
            roots,
            self.files_out.clone(),
        );
        search.search(self.app.generation(), String::new());
        self.search = Some(search);
    }

    /// Reads the open's tail when one is due: the own-segment history
    /// windows fill it, then the preview installs. No stream means the
    /// tail ended with nothing read; a lost read ends everything
    /// through `disconnected`, with no install. The tail read stays
    /// untimed: the attach timers cover the first fetched page below.
    /// Older windows and ancestors read after the first frame draws,
    /// through the retained count.
    fn open_tail(&mut self, rx: &Receiver<Input>) {
        let Some(session) = self.app.tail_due() else {
            return;
        };
        // A new open never installs or counts the old run.
        self.app.clear_tail_read();
        if !self.hub.has_stream() {
            self.app.tail_ended(None);
            self.app.install_tail();
            return;
        }
        let mut tail_lines_finished = self.clock.now();
        #[cfg(not(test))]
        let trace_diag = Some(self.trace_diag.as_ref());
        #[cfg(test)]
        let trace_diag = None;
        if self
            .hub
            .fetch_history(
                rx,
                &session,
                &mut self.app,
                &self.tick,
                self.clock.as_ref(),
                self.tick.armed(),
                trace_diag,
                Some(&mut tail_lines_finished),
                hub::Phase::Tail,
            )
            .is_err()
        {
            return;
        }
        // The completed read's install always runs: the open still holds
        // its tail read here (only a lost connection drops it, and that
        // returns above). No page count is compared: a rebuild can
        // hold the same count, and an empty tail still installs its
        // empty preview.
        #[cfg(not(test))]
        {
            let tail_to_install = log::diag::timed(
                &self.trace_diag,
                contract::diag::Op::TuiTranscript,
                Some("attach.tail_lines_to_install"),
                Some(tail_lines_finished),
            );
            drop(tail_to_install);
            let install = log::diag::timed(
                &self.trace_diag,
                contract::diag::Op::TuiTranscript,
                Some("attach.install_tail"),
                Some(self.clock.now()),
            );
            self.app.install_tail();
            drop(install);
            self.trace_tail_frame = Some(self.clock.now());
        }
        #[cfg(test)]
        self.app.install_tail();
    }

    /// Reads the retained count after the first frame draws: older
    /// own-segment windows to `seq 0`, then ancestors to the chain
    /// root, folded once above the preview at its end. A lost read ends
    /// everything through `disconnected`, with no count. One answered
    /// request at a time, as before the frame. With no count running
    /// nothing is asked and the count end is a no-op.
    fn count_tail(&mut self, rx: &Receiver<Input>) {
        let Some(session) = self.app.opening_session() else {
            return;
        };
        #[cfg(not(test))]
        let trace_diag = Some(self.trace_diag.as_ref());
        #[cfg(test)]
        let trace_diag = None;
        if self
            .hub
            .fetch_history(
                rx,
                &session,
                &mut self.app,
                &self.tick,
                self.clock.as_ref(),
                self.tick.armed(),
                trace_diag,
                None,
                hub::Phase::Count,
            )
            .is_err()
        {
            return;
        }
        // The completed count's fold always runs: the count still runs
        // here (only a lost connection drops it, and that returns
        // above).
        self.app.finish_count();
    }

    /// Loads, one page at a time, every page the frame needs and does not
    /// hold (`docs/tui.md`, "History and paging"). With no hub there is
    /// nothing to ask, and the pages draw blank.
    fn page_in(&mut self, rx: &Receiver<Input>) {
        while let Some((session, range)) = self.app.needs().into_iter().next() {
            if self.app.paging_session().is_none() {
                return;
            }
            if !self.hub.has_stream() {
                return;
            }
            // The first attach's first fetched page is timed once per
            // process, to completion whatever its outcome: an attempted
            // read is work. The guards above return before any attempt,
            // so they consume no latch.
            let log_read = self.timing.log_read();
            // Each range loads under its own segment's session: ancestor
            // and descendant logs both start at `Seq(0)`, so a bare range
            // names no page on its own.
            let fetched = self.hub.fetch(
                rx,
                &session,
                &range,
                &mut self.app,
                &self.tick,
                self.clock.as_ref(),
                self.tick.armed(),
            );
            drop(log_read);
            match fetched {
                Ok(lines) => {
                    // The first successful load folds the page into the
                    // transcript, once per process. A failed fetch never
                    // takes the latch, so it stays for the next load.
                    let transcript = self.timing.transcript();
                    self.app.load(&session, lines);
                    drop(transcript);
                    // An answer that folded nothing into the page would
                    // leave it needed, and the frame asking forever.
                    if self.app.needs().first() == Some(&(session.clone(), range.clone())) {
                        self.app
                            .load_failed(&session, &range, "the answer held none of its lines");
                    }
                }
                Err(message) => self.app.load_failed(&session, &range, &message),
            }
        }
    }

    /// Opens `url` with the terminal's opener: `open` or `xdg-open` on the
    /// terminal's machine. Over SSH or with no opener nothing launches;
    /// the URL is drawn either way and the flow completes when it is
    /// visited (`docs/tui.md`, "Links").
    fn open_link(&self, url: String) {
        if let Some(argv) = self.programs.open.clone() {
            let mut argv = argv;
            argv.push(url);
            drop(crate::helper::spawn_detached(
                argv,
                None,
                Arc::clone(&self.clock),
            ));
        }
    }

    /// Ctrl+G: opens `text` in the editor `$VISUAL` or `$EDITOR` names,
    /// with the terminal handed over, and gives the app what it returned.
    /// `Some(1)` when the terminal cannot be taken back. A shutdown signal
    /// while the editor runs quits with 128 plus its number instead, with
    /// no resume: the run's own restore takes the terminal back.
    fn open_editor(&mut self, target: editor::Target, text: &str) -> Option<i32> {
        let Some(command) = editor::command(&self.var) else {
            self.app
                .editor_returned(target, Err(editor::NO_EDITOR.to_owned()));
            return None;
        };
        let mut result = editor::Outcome::Done(Err(String::new()));
        let clock = Arc::clone(&self.clock);
        let code = self.hand_over(|| {
            result = editor::run(&command, text, &clock);
            // A shutdown stop quits outright, before any resume: the
            // handover takes it as the quit code.
            match result {
                editor::Outcome::Stopped(signal) => Some(128 + signal),
                editor::Outcome::Done(_) => None,
            }
        });
        if code.is_none() {
            // A stop converts to the quit code in the handover above,
            // so only Done reaches here: the draft is unchanged on a
            // kill, and live sessions keep their resume lines.
            if let editor::Outcome::Done(done) = result {
                self.app.editor_returned(target, done);
            }
        }
        code
    }

    /// Ctrl+G on a configuration view: opens `file` in the editor
    /// `$VISUAL` or `$EDITOR` names, with the terminal handed over, and
    /// tells the app it returned. `Some(1)` when the terminal cannot be
    /// taken back. A shutdown signal while the editor runs quits with 128
    /// plus its number instead, with no resume, as above.
    fn open_file(&mut self, file: &std::path::Path) -> Option<i32> {
        let Some(command) = editor::command(&self.var) else {
            self.app
                .config_file_closed(Err(editor::NO_EDITOR_FILE.to_owned()));
            return None;
        };
        let mut result = editor::Outcome::Done(Err(String::new()));
        let clock = Arc::clone(&self.clock);
        let code = self.hand_over(|| {
            result = editor::open(&command, file, &clock);
            // As in [`Loop::open_editor`]: a shutdown stop converts to
            // the quit code in this handover.
            match result {
                editor::Outcome::Stopped(signal) => Some(128 + signal),
                editor::Outcome::Done(_) => None,
            }
        });
        if code.is_none() {
            // A stop converts to the quit code in the handover above,
            // so only Done reaches here: quit with the signal's code,
            // closing nothing first, so live sessions keep their
            // resume lines.
            if let editor::Outcome::Done(done) = result {
                self.app.config_file_closed(done);
            }
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
            self.app.push_notice("config_invalid", notice);
        }
    }

    /// Hands the terminal to `program`, run in the foreground on this
    /// thread: the reader paused, the terminal restored, then both taken
    /// back and the whole screen repainted at the size read again. `Some(1)`
    /// when the terminal cannot be taken back.
    fn hand_over(&mut self, program: impl FnOnce() -> Option<i32>) -> Option<i32> {
        self.reader.pause();
        // A failed suspend still runs the program: the terminal may be
        // left part set up, and resume sets it up whole.
        term::suspend().unwrap_or(());
        // A shutdown stop quits outright: there is no terminal use to
        // resume for, and resuming can only fail the quit when the
        // terminal is gone. The reader still resumes, so its thread ends
        // with the test instead of parking past it.
        if let Some(code) = program() {
            self.reader.resume();
            return Some(code);
        }
        if term::resume(self.parser.kitty(), self.hover).is_err() {
            return Some(1);
        }
        // The restore popped the title: the next frame writes it again.
        self.title.forget();
        // The restore put the default pointer back: the next frame writes
        // the shape again.
        self.shape.reset();
        self.reader.resume();
        let (width, height) = term::size(&self.tty)
            .unwrap_or_else(|_| (self.screen.area().width, self.screen.area().height));
        self.app.set_size(width, height);
        self.screen.resize(width, height).err().map(|_| 1)
    }
}

#[cfg(test)]
#[path = "event_loop_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "event_loop_login_tests.rs"]
mod login_tests;

#[cfg(test)]
#[path = "event_loop_install_tests.rs"]
mod install_tests;

#[cfg(test)]
#[path = "event_loop_image_tests.rs"]
mod image_tests;

#[cfg(test)]
#[path = "event_loop_focus_tests.rs"]
mod focus_tests;

#[cfg(test)]
#[path = "event_loop_loop_tests.rs"]
mod loop_tests;

#[cfg(test)]
#[path = "timing_tests.rs"]
mod timing_tests;

#[cfg(test)]
#[path = "event_loop_mouse_tests.rs"]
mod mouse_tests;

#[cfg(test)]
#[path = "event_loop_delegates_tests.rs"]
mod delegates_tests;
#[cfg(test)]
#[path = "event_loop_panel_tests.rs"]
mod panel_tests;
#[cfg(test)]
#[path = "event_loop_rail_tests.rs"]
mod rail_tests;

#[cfg(test)]
#[path = "event_loop_drag_tests.rs"]
mod drag_tests;

#[cfg(test)]
#[path = "event_loop_attention_tests.rs"]
mod attention_tests;

#[cfg(test)]
#[path = "event_loop_look_tests.rs"]
mod look_tests;

#[cfg(test)]
#[path = "event_loop_settings_tests.rs"]
mod settings_tests;

#[cfg(test)]
#[path = "event_loop_paste_tests.rs"]
mod paste_tests;

#[cfg(test)]
#[path = "event_loop_reconnect_tests.rs"]
mod reconnect_tests;

#[cfg(test)]
#[path = "remote_run_tests.rs"]
mod remote_run_tests;
