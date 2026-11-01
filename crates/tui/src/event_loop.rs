//! The event loop (`docs/tui.md`, "History and paging"): `run` sets the
//! terminal up, draws the first frame and starts the input threads; the loop
//! handles one batch of ready inputs at a time, fetches the history pages a frame needs
//! before drawing it, and hands the terminal to the editor.

pub(crate) mod batch;
mod hub;
pub(crate) mod kitty_writer;
#[cfg(test)]
mod pause;

use batch::batchable;
use hub::Connection;
use kitty_writer::{Item, KittyWriter, TtyLock};

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};

use contract::clock::Clock;
use contract::{ErrorCode, SessionId};
use ratatui::backend::Backend;
use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGWINCH};
use signal_hook::iterator::Signals;

use crate::app::{App, Effect};
use crate::catalogue;
use crate::graphics::logo;
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
    // The attach setup span opens beside it, and every return that
    // discards the attached frame discards the attach spans with it.
    let mut timing = crate::timing::Timing::new(diag);
    let first_render = timing.first_render();
    let attached_frame = timing.attached_frame();
    timing.start_attach_setup();
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
        crate::timing::discard(attached_frame);
        timing.discard_attach_spans();
        return Ok(1);
    };
    catch_interrupts();
    let restore = term::Guard;
    // The first frame waits for the opening pass and local TUI loading,
    // never a child process or a tty/hub read.
    let hover = launch.hover;
    let Ok((width, height)) = term::setup(&tty, hover) else {
        crate::timing::discard(first_render);
        crate::timing::discard(attached_frame);
        timing.discard_attach_spans();
        return Ok(1);
    };
    let Ok(out) = tty.try_clone() else {
        crate::timing::discard(first_render);
        crate::timing::discard(attached_frame);
        timing.discard_attach_spans();
        return Ok(1);
    };
    let Ok(mut screen) = Screen::new(crate::screen::backend(out), width, height) else {
        crate::timing::discard(first_render);
        crate::timing::discard(attached_frame);
        timing.discard_attach_spans();
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
    app.set_logo_depth(look.depth());
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
    // The doctor report reader `main` injected: taken out beside the
    // writer, for the same reason.
    app.set_doctor(std::mem::take(&mut launch.doctor));
    // With no model configured the picker opens on home, before the
    // first frame: saving the choice writes the config default. It stays
    // shut when attaching to a session.
    let pick_at_start = launch.model.is_none() && launch.open_at == crate::OpenAt::Home;
    // The viewer's copies live under Fiber home, taken before home
    // keeps the launch description.
    let images_dir = launch.images.clone();
    // The image converter runs the launch's executable again: taken
    // before home keeps the launch description, so a stored JPEG or
    // WebP converts to PNG before Kitty transmission (`docs/tui.md`,
    // "Images").
    let converter = launch.fiber.clone().map(|fiber| {
        vec![
            fiber.into_os_string(),
            OsString::from("image"),
            OsString::from("--png"),
            OsString::from("/dev/stdin"),
        ]
    });
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
    // first frame already sized (`docs/tui.md`, "Images"). The measured
    // cell rides beside it for the logo's Sixel gate, never the
    // fallback (`docs/tui.md`, "The logo").
    app.set_cell(term::cell(term::pixels(&tty), (width, height)));
    app.set_measured_cell(term::measured_cell(term::pixels(&tty), (width, height)));
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
        converter,
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
        // The load is timed but never moved: extensions may change the
        // first frame, so they load before it draws (`docs/tui.md`,
        // "How a TUI extension runs").
        let load = timing.extensions();
        let (renderer, notices) = loader.load();
        drop(load);
        app.use_renderer(renderer);
        for (code, text) in notices {
            app.push_notice(&code, text);
        }
    }
    if screen.draw(&mut app, None).is_err() {
        crate::timing::discard(first_render);
        crate::timing::discard(attached_frame);
        timing.discard_attach_spans();
        return Ok(1);
    }
    // The frame drew: its line stands, whatever the loop meets after.
    // The setup span ends here and the wait span opens: the initial
    // unattached draw is complete.
    drop(first_render);
    timing.stop_attach_setup();
    timing.start_attach_wait();
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
    let reader = match spawn_reader(&tty, tx.clone()) {
        Ok(reader) => reader,
        Err(error) => {
            crate::timing::discard(attached_frame);
            timing.discard_attach_spans();
            return Err(error.into());
        }
    };
    let mut terminal = Loop {
        app,
        parser: Parser::default(),
        screen,
        logo: logo::Emit::default(),
        hub: Connection::new(Arc::clone(&retry)),
        kitty_writer: None,
        tty_lock: Arc::new(TtyLock::new()),
        kitty_failed: Arc::new(AtomicBool::new(false)),
        handover_retained: Vec::new(),
        pending_resume: false,
        tty,
        on_attach,
        clock,
        timing,
        attached_frame,
        summary_span: None,
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
    if let Err(error) =
        terminal
            .tick
            .start_with(Arc::clone(&terminal.clock), tx.clone(), spawn_tick)
    {
        crate::timing::discard(terminal.attached_frame.take());
        terminal.timing.discard_attach_spans();
        return Err(error.into());
    }
    terminal.tick.arm(terminal.app.take_wake());
    spawn_signal(term_signals, tx.clone());
    if let Some(signals) = signals {
        spawn_resize(signals, tx);
    }
    let code = terminal.run(&rx);
    crate::timing::discard(terminal.attached_frame.take());
    terminal.timing.discard_attach_spans();
    terminal.tick.stop();
    // The writer goes before the hub hangs up and the guard drops: Stop
    // sends its mandatory abort delete at the next chunk boundary, then
    // drops queued payloads and joins.
    terminal.stop_kitty();
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
    /// The image converter's argv, from the launch's executable path:
    /// `None` leaves JPEG and WebP their one line (`docs/tui.md`,
    /// "Images").
    converter: Option<Vec<OsString>>,
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
    /// The logo's emission: once per theme per home visit
    /// (`docs/tui.md`, "The logo").
    logo: logo::Emit,
    /// The hub connection: the stream, the held inputs and the hub
    /// thread's permit, reached only through its interface.
    hub: Connection,
    /// The image writer thread, spawned lazily on the first non-empty
    /// handoff: the sole owner of its teardown.
    kitty_writer: Option<KittyWriter>,
    /// The terminal-output lock arbitrating loop writes against writer
    /// chunks. Guards cover actual tty writes only, never event dispatch.
    tty_lock: Arc<TtyLock>,
    /// Set when a tty write fails: the loop takes the terminal-failure
    /// path from the next frame top, and an idle loop wakes through
    /// [`Input::KittyFailed`].
    kitty_failed: Arc<AtomicBool>,
    /// Image ids the paused writer still holds, taken by `hand_over` for
    /// the post-resume replay.
    handover_retained: Vec<u32>,
    /// Set after a successful resume: the first post-resume frame sends
    /// Resume after its draw.
    pending_resume: bool,
    /// The tty, for its size on resize.
    tty: File,
    on_attach: OnAttach,
    clock: Arc<dyn Clock>,
    /// The diagnostic writer the timed scopes share.
    timing: crate::timing::Timing,
    /// The first draw with attached-session history, carried from
    /// `run_with`; discarded if the loop ends before that draw.
    attached_frame: Option<log::diag::Timer>,
    /// The opening summary subscribe's guard, while the open holds that
    /// stage: dropped once the open leaves it, writing one line.
    summary_span: Option<log::diag::Timer>,
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
        let result = self.handle_events_inner(events);
        self.settle_and_send(result)
    }

    /// Settles a Loop boundary and sends the outgoing lines unless it quits.
    fn settle_and_send(&mut self, result: Option<i32>) -> Option<i32> {
        let lines = self.app.settle_and_find_outgoing_if_up();
        if result.is_none()
            && let Some(unsent) = self.hub.send(&lines, &mut self.app)
        {
            self.handle(Input::WriteFailed(unsent));
        }
        result
    }

    /// Sends lines during an input route, before that input's settlement.
    fn send_inner(&mut self, lines: &[String]) {
        if let Some(unsent) = self.hub.send(lines, &mut self.app) {
            self.app.write_failed_inner(&unsent);
        }
    }

    /// Dispatches parsed events without settling; the input boundary owns the layout pass.
    fn handle_events_inner(&mut self, events: Vec<Event>) -> Option<i32> {
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
                self.send_inner(&[line]);
            }
            let effect = match event {
                Event::Stroke(stroke) => self.app.on_press_inner(stroke, self.clock.now()).0,
                Event::Key(key) => self.app.on_key_inner(key, self.clock.now()),
                Event::Edit(edit) => self.app.on_edit_inner(edit),
                Event::Mouse(mouse) => self.app.on_mouse_inner(
                    &mouse,
                    self.screen.targets(),
                    &mut self.pointer,
                    self.hover,
                    self.clock.now(),
                ),
                Event::Reply(Reply::KittyFlags(_)) => {
                    let lock = Arc::clone(&self.tty_lock);
                    let _guard = lock.loop_write();
                    self.kitty();
                    Effect::None
                }
                Event::Reply(Reply::Appearance(appearance)) => {
                    self.screen.appearance(appearance);
                    Effect::None
                }
                Event::Reply(Reply::Graphics { id, ok }) => {
                    if id == logo::ID {
                        // The logo's own transmit: a rejection leaves the
                        // pixel logo with no notice and no retry, touching
                        // no image state (`docs/tui.md`, "The logo").
                        if !ok {
                            self.app.set_logo_rejected();
                        }
                    } else {
                        self.graphics(id, ok);
                    }
                    Effect::None
                }
                Event::Reply(Reply::Version(text)) => {
                    self.version(text);
                    Effect::None
                }
                Event::Reply(Reply::DeviceAttributes { sixel }) => {
                    if sixel {
                        self.app.set_sixel();
                    }
                    Effect::None
                }
                Event::Focus(_) => Effect::None,
            };
            match effect {
                Effect::None => {}
                Effect::Copy(text) => {
                    let lock = Arc::clone(&self.tty_lock);
                    let _guard = lock.loop_write();
                    clipboard::copy(
                        Some(&self.tty),
                        self.programs.copy.as_deref(),
                        text,
                        &self.clock,
                    );
                }
                Effect::OpenLink(url) => self.open_link(url),
                Effect::Send(lines) => self.send_inner(&lines),
                Effect::Quit => return Some(0),
                Effect::Exit(lines) => {
                    // The quit question's closes go out, then the
                    // terminal quits: a close never written keeps
                    // its resume line.
                    self.send_inner(&lines);
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
                        self.app.on_image_inner(ticket, Err(notice));
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
                        self.app.on_feedback_inner(Err(notice));
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
        match input {
            Input::WriteFailed(unsent) => {
                self.app.set_now(
                    self.clock.now(),
                    contract::clock::wall_ms(self.clock.wall()),
                );
                self.app.on_write_failed(&unsent);
                self.save_shares();
                None
            }
            input @ (Input::Bytes(_)
            | Input::Hub(_)
            | Input::Connected(..)
            | Input::ConnectFailed(_)
            | Input::Disconnected
            | Input::Resize
            | Input::Files { .. }
            | Input::FindDue(_)
            | Input::Image { .. }
            | Input::Viewed { .. }
            | Input::Converted { .. }
            | Input::Models(_)
            | Input::Feedback(_)
            | Input::Login { .. }
            | Input::Install { .. }
            | Input::HooksOfferNames { .. }
            | Input::HooksInstalled { .. }
            | Input::Tick
            | Input::Signal(_)
            | Input::KittyFailed) => {
                let result = self.handle_inner(input);
                self.settle_and_send(result)
            }
        }
    }

    /// Dispatches one input without settling; the input boundary owns the layout pass.
    fn handle_inner(&mut self, input: Input) -> Option<i32> {
        self.app.set_now(
            self.clock.now(),
            contract::clock::wall_ms(self.clock.wall()),
        );
        let before = self.app.session().cloned();
        match input {
            // A signal quits with 128 plus its number, closing nothing
            // first: live sessions keep their resume lines.
            Input::Signal(signal) => return Some(128 + signal),
            // A failed Kitty write drops every pending image and writes
            // nothing further: the loop quits through the failed-draw
            // exit, closing nothing first.
            Input::KittyFailed => return Some(1),
            Input::Bytes(bytes) => {
                let events = self.parser.feed(&bytes);
                return self.handle_events_inner(events);
            }
            Input::Hub(line) => {
                let lines = self.app.on_line_inner(line);
                self.send_inner(&lines);
            }
            Input::Connected(stream, hello) => {
                self.hub.store(stream);
                // A stream whose write timeout would not set is shut
                // down, not adopted: the connection failed, as a refused
                // connect does. The reader's end of it retries like any
                // other lost connection.
                if !self.hub.has_stream() {
                    self.app.connect_failed_inner(
                        "Could not set up the hub connection: its write timeout could not be set"
                            .to_owned(),
                    );
                } else {
                    let lines = self.app.on_line_inner(Line::Hub(hello));
                    // The summary span starts before the send, so the
                    // send through the acceptance is covered; the frame
                    // top keeps it and drops it after it folds.
                    self.poll_summary_span();
                    self.send_inner(&lines);
                    // A `hub_hello` this terminal cannot read leaves it
                    // unconnected: it says so and disconnects.
                    if !self.app.connected() {
                        self.hub.hang_up();
                    }
                }
            }
            Input::ConnectFailed(error) => {
                self.app
                    .connect_failed_inner(format!("Could not reach the hub: {error}"));
                self.hub.retry_later(&mut self.app);
            }
            Input::Disconnected => self.hub.on_disconnected(&mut self.app),
            Input::WriteFailed(unsent) => self.app.write_failed_inner(&unsent),
            Input::Files { generation, result } => self.app.on_files_inner(generation, result),
            Input::Models(result) => {
                self.app.on_models_inner(result);
                // The queued read, if one waits, starts on the loop's
                // channel.
                self.model_reader.done(&self.files_out);
            }
            Input::Image { ticket, result } => self.app.on_image_inner(ticket, result),
            Input::Feedback(result) => self.app.on_feedback_inner(result),
            Input::Viewed {
                id,
                name,
                generation,
                result,
            } => self.app.image_viewed(id, &name, generation, result),
            Input::Converted {
                generation,
                id,
                result,
            } => {
                self.app.image_converted(generation, id, result);
                self.start_next_convert();
            }
            Input::FindDue(generation) => {
                let lines = self.app.find_due(generation);
                self.send_inner(&lines);
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
                self.send_inner(&lines);
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
                    self.app.set_measured_cell(term::measured_cell(
                        term::pixels(&self.tty),
                        (width, height),
                    ));
                    self.app.set_size_inner(width, height);
                    let lock = Arc::clone(&self.tty_lock);
                    let _guard = lock.loop_write();
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
        let result = self.frame_inner(rx);
        self.settle_and_send(result)
    }

    /// Runs the frame without settling; the frame boundary owns the layout pass.
    fn frame_inner(&mut self, rx: &Receiver<Input>) -> Option<i32> {
        // A failed Kitty write ends the loop from the next frame top: no
        // further frame, title, shape, alert or delete write starts. A
        // loop write already running may be attempted before this check,
        // but a failed tty contributes no bytes.
        if self.kitty_failed.load(std::sync::atomic::Ordering::SeqCst) {
            return Some(1);
        }
        // The summary span opens and closes here: every handled batch
        // ends in a frame, so the send is seen on the next frame top
        // and the acceptance on the frame after it folds.
        self.poll_summary_span();
        // A frame past its 16 ms budget writes `tui.frame`; one within
        // budget writes no line, so scrolling leaves the log small.
        let _frame = self.timing.frame();
        // The reconciler's subscribes go out before the frame pages: an
        // acknowledgement arriving late is reconciled on the step that
        // brings it.
        let now = self.clock.now();
        let due = self.app.items_due(now);
        self.send_inner(&due);
        // A rewind `history` wait past its bound ends as a lost
        // connection, exactly as the loop's own history wait does.
        if self.app.rewind_due(now) {
            self.hub.lost(&mut self.app);
        }
        // The open's tail reads before the frame pages: the first frame
        // draws the preview it installs.
        let attached_history = self.open_tail(rx);
        self.page_in(rx);
        self.apply_theme();
        // A selection's copy waiting on dropped pages runs once they load,
        // and a `/copy` waiting on an opening pass once the pass ends.
        if let Some(text) = self.app.take_copy() {
            let lock = Arc::clone(&self.tty_lock);
            let _guard = lock.loop_write();
            clipboard::copy(
                Some(&self.tty),
                self.programs.copy.as_deref(),
                text,
                &self.clock,
            );
        }
        if let Some(text) = self.app.take_owed_copy() {
            let lock = Arc::clone(&self.tty_lock);
            let _guard = lock.loop_write();
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
            let lock = Arc::clone(&self.tty_lock);
            let _guard = lock.loop_write();
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
        // The attached-history draw ends the prepare span and opens the
        // draw span immediately before it; any other frame draws outside
        // both.
        if attached_history {
            self.timing.stop_attach_prepare();
            self.timing.start_attach_draw();
        }
        let lock = Arc::clone(&self.tty_lock);
        let draw_failed = {
            let _guard = lock.loop_write();
            self.screen.draw(&mut self.app, self.pointer.at).is_err()
        };
        if draw_failed {
            self.timing.discard_attach_spans();
            return Some(1);
        }
        // The attached-session history has landed in the drawn frame:
        // the tail read completed and installed its preview before this
        // draw. A frame before that install leaves the guard open;
        // run_with discards it if no such frame arrives.
        if attached_history {
            // The completed draw ends the draw span and writes all five
            // retained spans, before the parent frame line.
            self.timing.stop_attach_draw();
            self.timing.emit_attach_spans();
            drop(self.attached_frame.take());
        }
        // After the first post-handover draw, the writer restarts its
        // retained images whole; the re-fetched requests drain below.
        if self.pending_resume {
            self.pending_resume = false;
            if let Some(writer) = &self.kitty_writer {
                writer.resume_after_handover();
            }
        }
        // The retained count reads after the first frame draws and
        // before the `full` raise goes out below, so no ancestor window
        // is asked before the own-segment screen shows.
        self.count_tail(rx);
        // The display fetches the frame drew go out after it, with the
        // kitty bytes their answers queued (`docs/tui.md`, "Images").
        self.drain_display();
        // The logo's image follows the drawn frame: its bytes position
        // at the drawn rect, or a lost rect repaints and redraws at once
        // (`docs/tui.md`, "The logo").
        if let Some(code) = self.logo_step() {
            return Some(code);
        }
        self.app.sweep_notices(self.clock.now());
        // The open's `full` raise goes out after the first frame draws,
        // so no `full` line is written before it.
        if let Some(line) = self.app.take_raise() {
            self.send_inner(&[line]);
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

    /// Hands the queued session-change deletes to the writer before the
    /// screen draw, then waits for them to reach the wire: a later session
    /// reuses image ids from 1, so its first frame never draws under a
    /// stale image (`docs/tui.md`, "Images"). The ack means every delete
    /// up to the barrier is on the wire, not that payloads are drained;
    /// its bound is at most two chunk writes plus scheduling.
    fn drain_image_deletes(&mut self) {
        let deletes = self.app.take_image_deletes();
        if deletes.is_empty() {
            return;
        }
        if !self.ensure_kitty_writer() {
            return;
        }
        let Some(writer) = &self.kitty_writer else {
            return;
        };
        for delete in deletes {
            if !writer.push(Item::Delete(delete)) {
                self.kitty_failed();
                return;
            }
        }
        // The first post-handover frame restarts the writer after its
        // draw: the writer is still parked, so no barrier waits here, or
        // the frame would never reach the resume. The push above already
        // queued the deletes for the replay.
        if self.pending_resume {
            return;
        }
        if !writer.barrier() {
            self.kitty_failed();
        }
    }

    /// Sends the display `read_file` commands the frame drew, and hands
    /// the kitty transmit and place items their answers queued to the
    /// writer thread, after the screen draw (`docs/tui.md`, "Images").
    /// The purge a session change queued goes before later items. Then
    /// starts the next queued converter while the one worker is free,
    /// without waiting on the child.
    fn drain_display(&mut self) {
        let out = self.app.take_image_out();
        self.send_inner(&out.display);
        if out.purge || !out.items.is_empty() {
            if !self.ensure_kitty_writer() {
                self.start_next_convert();
                return;
            }
            let Some(writer) = &self.kitty_writer else {
                self.start_next_convert();
                return;
            };
            if out.purge && !writer.push_purge() {
                self.kitty_failed();
                self.start_next_convert();
                return;
            }
            for item in out.items {
                if !writer.push(item) {
                    self.kitty_failed();
                    break;
                }
            }
        }
        self.start_next_convert();
    }

    /// Spawns the writer on the first non-empty handoff, cloning the
    /// loop's current tty then. Empty handoffs never spawn. False when the
    /// tty cannot be cloned or the thread cannot start: the terminal is
    /// failing, so the loop takes the terminal-failure path.
    fn ensure_kitty_writer(&mut self) -> bool {
        if self.kitty_writer.is_some() {
            return true;
        }
        let Ok(tty) = self.tty.try_clone() else {
            self.kitty_failed();
            return false;
        };
        let gate = Arc::new(kitty_writer::KittyChunkGate::new(false));
        let Some(writer) = KittyWriter::spawn(
            tty,
            gate,
            Arc::clone(&self.kitty_failed),
            self.files_out.clone(),
            Arc::clone(&self.tty_lock),
        ) else {
            self.kitty_failed();
            return false;
        };
        self.kitty_writer = Some(writer);
        true
    }

    /// Takes the terminal-failure path from the loop side: the flag the
    /// next frame top checks, plus a best-effort wake so an idle loop
    /// quits without waiting on its channel.
    fn kitty_failed(&self) {
        self.kitty_failed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        drop(self.files_out.send(Input::KittyFailed));
    }

    /// Sends Stop and joins the writer before the hub hangs up and the
    /// guard drops, so no payload or thread outlives the loop. Joining can
    /// wait for the current chunk's blocking write or write-readiness poll.
    fn stop_kitty(&mut self) {
        if let Some(writer) = self.kitty_writer.take() {
            writer.stop_and_join();
        }
    }

    /// Starts the next queued conversion when the one worker is free:
    /// the worker answers with `Input::Converted`, which frees the slot
    /// and starts the next. Without an executable path the queued
    /// conversion fails at once, freeing the slot for the rest of the
    /// queue (`docs/tui.md`, "Images").
    fn start_next_convert(&mut self) {
        while let Some(convert) = self.app.take_next_convert() {
            let Some(argv) = self.programs.converter.clone() else {
                self.app.image_converted(
                    convert.generation,
                    convert.id,
                    Err("image conversion failed".to_owned()),
                );
                continue;
            };
            crate::convert::spawn(&argv, convert, &self.files_out, crate::convert::OUTPUT_CAP);
            return;
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
            let lock = Arc::clone(&self.tty_lock);
            let _guard = lock.loop_write();
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
            let lock = Arc::clone(&self.tty_lock);
            let _guard = lock.loop_write();
            let mut tty = &self.tty;
            tty.write_all(bytes).unwrap_or(());
        }
    }

    /// Writes the attention bytes this input queued (`docs/tui.md`,
    /// "Getting the person's attention"). A failed write is dropped.
    fn write_alerts(&mut self) {
        let bytes = self.app.take_alerts();
        if bytes.is_empty() {
            return;
        }
        let lock = Arc::clone(&self.tty_lock);
        let _guard = lock.loop_write();
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

    /// An XTVERSION reply: an iTerm2-family version names iTerm2's
    /// image protocol, so the logo may draw as an image
    /// (`docs/tui.md`, "The logo"). Any other text changes nothing.
    fn version(&mut self, text: String) {
        if text.starts_with("iTerm2 ") || text.starts_with("WezTerm ") {
            self.app.set_iterm();
        }
    }

    /// Decides this frame's logo emission for the drawn home: the
    /// protocol from the drawn form, the rect, the tint, and whether
    /// the frame wrote.
    fn logo_after(&mut self, repainted: bool) -> (logo::Protocol, logo::After) {
        let rect = crate::view::logo_rect(&self.app, self.screen.area());
        let shown = self.app.home_screen().map(|screen| screen.logo);
        let protocol = match shown {
            Some(logo::Shown::Kitty) => logo::Protocol::Kitty,
            Some(logo::Shown::Blank) => {
                let sixel = if self.app.sixel() {
                    self.app.measured_cell().filter(|cell| logo::cell_ok(*cell))
                } else {
                    None
                };
                logo::select(false, self.app.iterm(), sixel)
            }
            _ => logo::Protocol::None,
        };
        let tint = self
            .screen
            .logo_tint()
            .map(|(tint, (red, green, blue))| (tint, [red, green, blue]));
        let touched = self.screen.take_touched();
        let after = self
            .logo
            .after_frame(protocol, rect, touched, repainted, tint);
        (protocol, after)
    }

    /// The logo's image after the drawn frame: its transmit or
    /// positioned write goes out for the drawn rect, or a lost rect
    /// repaints and the current screen redraws at once. Returns the
    /// quit code when a tty write fails (`docs/tui.md`, "The logo").
    fn logo_step(&mut self) -> Option<i32> {
        let (protocol, after) = self.logo_after(false);
        if after.repaint {
            let lock = Arc::clone(&self.tty_lock);
            let _guard = lock.loop_write();
            if self.screen.repaint().is_err() {
                return Some(1);
            }
            if self.screen.draw(&mut self.app, self.pointer.at).is_err() {
                return Some(1);
            }
            let (protocol, after) = self.logo_after(true);
            return self.logo_write(protocol, after);
        }
        self.logo_write(protocol, after)
    }

    /// Writes one logo emission: the kitty transmit beside its place
    /// bytes through the chunk writer, else the positioned write
    /// straight to the tty.
    fn logo_write(&mut self, protocol: logo::Protocol, after: logo::After) -> Option<i32> {
        if after.bytes.is_empty() {
            return None;
        }
        if protocol == logo::Protocol::Kitty {
            let (tint, _) = self.screen.logo_tint()?;
            if !self.ensure_kitty_writer() {
                return None;
            }
            let chunks = Self::kitty_logo_chunks(&logo::kitty(&tint));
            let place = after.bytes;
            let Some(writer) = &self.kitty_writer else {
                return None;
            };
            // Best-effort: a dead writer already reported `KittyFailed`,
            // which quits the loop, so the logo returns `None` either way.
            let _ = writer.push(Item::Image {
                id: logo::ID,
                chunks,
                place,
            });
            return None;
        }
        let lock = Arc::clone(&self.tty_lock);
        let _guard = lock.loop_write();
        if self.tty.write_all(&after.bytes).is_err() {
            return Some(1);
        }
        None
    }

    /// Splits the logo's base64 payload into kitty transmit chunks: the
    /// first names the RGBA raster under the logo's id, the rest
    /// continue it, `m=1` on all but the last, so the writer thread
    /// sends each chunk between loop writes without splitting an
    /// escape (`docs/tui.md`, "Images", "The logo").
    fn kitty_logo_chunks(payload: &[u8]) -> Vec<Vec<u8>> {
        const CHUNK: usize = 4096;
        let mut out = Vec::new();
        let mut pieces = payload.chunks(CHUNK).peekable();
        if pieces.peek().is_none() {
            return out;
        }
        let mut first = true;
        while let Some(piece) = pieces.next() {
            let more = u8::from(pieces.peek().is_some());
            let mut chunk = Vec::new();
            if first {
                chunk.extend_from_slice(
                    format!(
                        "\x1b_Ga=t,f=32,s={},v={},i={},q=2,m={more};",
                        logo::WIDTH,
                        logo::HEIGHT,
                        logo::ID
                    )
                    .as_bytes(),
                );
                first = false;
            } else {
                chunk.extend_from_slice(format!("\x1b_Gm={more};").as_bytes());
            }
            chunk.extend_from_slice(piece);
            chunk.extend_from_slice(b"\x1b\\");
            out.push(chunk);
        }
        out
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

    /// Starts the attach summary guard while the open's first-stage
    /// summary subscribe is in flight and drops it once the open leaves
    /// that stage, writing one `tui.attach_summary` line per process.
    /// A second open finds the latch set and writes nothing. The guard
    /// only observes: the subscribes and their answers are unchanged.
    fn poll_summary_span(&mut self) {
        if self.app.summary_in_flight() {
            if self.summary_span.is_none() {
                self.summary_span = self.timing.attach_summary();
            }
            return;
        }
        if let Some(span) = self.summary_span.take() {
            drop(span);
        }
    }

    /// Reads the open's tail when one is due: the own-segment history
    /// windows fill it, then the preview installs. No stream means the
    /// tail ended with nothing read; a lost read ends everything
    /// through `disconnected`, with no install. Returns true when the
    /// live-stream tail read reaches its install, so the draw can end the
    /// attached-frame guard; a missing stream or lost read returns false.
    /// The tail wait, each answer fold and the install are timed once per
    /// process; the first fetched page below keeps its own timer. Older
    /// windows and ancestors read after the first frame draws, through the
    /// retained count.
    fn open_tail(&mut self, rx: &Receiver<Input>) -> bool {
        let Some(session) = self.app.tail_due() else {
            return false;
        };
        // The first due attempt ends the wait span and opens the tail
        // span; a later attempt keeps the first attempt's guard, so any
        // lost-attempt interval between stays inside the span.
        self.timing.stop_attach_wait();
        self.timing.start_attach_tail();
        // A new open never installs or counts the old run.
        self.app.clear_tail_read();
        if !self.hub.has_stream() {
            self.app.tail_ended(None);
            self.app.install_tail();
            return false;
        }
        if self
            .hub
            .fetch_history(
                rx,
                &session,
                &mut self.app,
                &self.tick,
                self.clock.as_ref(),
                self.tick.armed(),
                hub::Phase::Tail,
                &mut self.timing,
            )
            .is_err()
        {
            return false;
        }
        // The completed read's install always runs: the open still holds
        // its tail read here (only a lost connection drops it, and that
        // returns above). No page count is compared: a rebuild can
        // hold the same count, and an empty tail still installs its
        // empty preview. The install holds its own guard: it installs
        // the newest candidate without another fold (`docs/tui.md`,
        // "History and paging"); when no answer folded one, it folds
        // the leftover candidate first, under the same guard.
        let install = self.timing.tail_install();
        self.app.install_tail();
        drop(install);
        // The install the attempt reached ends the tail span and opens
        // the prepare span: reopened and rejected-history installs count,
        // as the read completed through them.
        self.timing.stop_attach_tail();
        self.timing.start_attach_prepare();
        true
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
        if self
            .hub
            .fetch_history(
                rx,
                &session,
                &mut self.app,
                &self.tick,
                self.clock.as_ref(),
                self.tick.armed(),
                hub::Phase::Count,
                &mut self.timing,
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
                    self.app.load_inner(&session, lines);
                    drop(transcript);
                    // An answer that folded nothing into the page would
                    // leave it needed, and the frame asking forever.
                    if self.app.needs().first() == Some(&(session.clone(), range.clone())) {
                        self.app.load_failed_inner(
                            &session,
                            &range,
                            "the answer held none of its lines",
                        );
                    }
                }
                Err(message) => self.app.load_failed_inner(&session, &range, &message),
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
        // The writer pauses before the loop takes its terminal-output
        // priority: the pause closes a partial upload at the next chunk
        // boundary before `term::suspend` writes `1049l`.
        self.pause_kitty();
        let lock = Arc::clone(&self.tty_lock);
        let _guard = lock.loop_write();
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
                self.app.editor_returned_inner(target, done);
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
        // As in [`Loop::open_editor`]: pause the writer first so no upload
        // stays open across the foreground program.
        self.pause_kitty();
        let lock = Arc::clone(&self.tty_lock);
        let _guard = lock.loop_write();
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
        self.app.set_logo_depth(look.depth());
        self.screen.set_look(look);
        if let Some(notice) = notice {
            self.app.push_notice("config_invalid", notice);
        }
    }

    /// Pauses the writer before handing the terminal over: a partial upload
    /// closes at the next chunk boundary, so no upload stays open across
    /// the foreground program. The retained ids wait for `hand_over` to
    /// take after a successful resume.
    fn pause_kitty(&mut self) {
        // Idempotent while Resume is pending: a second handover in the same
        // batch skips a second writer Handover, reusing the retained id set
        // until the first post-resume frame sends Resume. Without this the
        // second pause would wait for an ack the parked writer holds until
        // that Resume.
        if self.pending_resume {
            return;
        }
        let Some(writer) = &self.kitty_writer else {
            return;
        };
        match writer.pause_for_handover() {
            Some(ids) => self.handover_retained = ids,
            None => self.kitty_failed(),
        }
    }

    /// Hands the terminal to `program`, run in the foreground on this
    /// thread: the reader paused, the terminal restored, then both taken
    /// back and the whole screen repainted at the size read again. `Some(1)`
    /// when the terminal cannot be taken back. The caller holds the
    /// loop-priority terminal-output guard across the handover, which the
    /// resize below reuses. After a successful resume the writer's retained
    /// images stay shown while dropped ones are re-asked, and the first
    /// post-resume frame restarts the writer.
    /// After a successful resume, keeps writer-retained images shown and
    /// re-asks dropped ones, once: a second handover before the first
    /// post-resume frame must not reset again, or the retained images the
    /// parked writer will replay are requested and sent twice.
    fn note_handover_resumed(&mut self) {
        if self.pending_resume {
            return;
        }
        let retained = std::mem::take(&mut self.handover_retained);
        self.app.images_after_handover(retained);
        self.pending_resume = true;
    }

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
            // A failed resume or a shutdown leaves the writer paused for
            // Stop and join: no Resume is sent.
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
        self.note_handover_resumed();
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

/// The attach spans' event-loop wiring (`docs/state.md`, "Diagnostic
/// logs"): the due tail attempt ends the wait and opens the tail, the
/// install that attempt reaches ends the tail and opens the prepare,
/// and the attached-history draw ends the prepare, covers the draw and
/// writes all five. A false attempt retains its spans without writing,
/// and a parent discard writes none. The timers read the diagnostic
/// writer's stepping clock, so each span's milliseconds pin its exact
/// boundary sequence with no sleep.
#[cfg(test)]
mod attach_span_tests {
    use std::io::BufRead;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant, SystemTime};

    use contract::SessionId;
    use contract::clock::Clock;
    use fakes::clock::FakeClock;
    use ratatui::backend::TestBackend;

    use super::Input;
    use super::tests::{DEADLINE, new_loop};
    use crate::link::Line;

    /// The session every attach test opens.
    const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

    /// A clock stepping 20 ms on every monotonic read, so each stamped
    /// boundary lands on its own instant with no sleep.
    struct StepClock {
        base: Instant,
        reads: AtomicU64,
    }

    impl StepClock {
        fn stepped() -> Arc<Self> {
            Arc::new(Self {
                base: FakeClock::new().now(),
                reads: AtomicU64::new(0),
            })
        }
    }

    impl Clock for StepClock {
        fn now(&self) -> Instant {
            let read = self.reads.fetch_add(1, Ordering::SeqCst);
            self.base + Duration::from_millis(20) * u32::try_from(read).unwrap_or(u32::MAX)
        }

        fn wall(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
        }

        fn sleep(&self, _d: Duration) {}

        fn wait_until(&self, _until: Option<Instant>, _wait: &mut dyn FnMut(Option<Duration>)) {}

        fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
    }

    /// A debug writer in `home` on the stepping clock.
    fn debug_diag(home: &std::path::Path) -> Arc<log::diag::Diag> {
        let clock: Arc<dyn Clock> = StepClock::stepped();
        Arc::new(log::diag::Diag::new(
            home,
            log::diag::Process::Tui,
            log::diag::Level::Debug,
            clock,
        ))
    }

    /// Every `op_timing` line in the process's diagnostic file, in file
    /// order; none when nothing was ever emitted there.
    fn op_lines(home: &std::path::Path) -> Vec<serde_json::Value> {
        let path = home
            .join("logs")
            .join(format!("tui-{}.log", std::process::id()));
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        text.lines()
            .map(|line| {
                serde_json::from_str(line).unwrap_or_else(|err| panic!("a line parses: {err}"))
            })
            .collect()
    }

    /// The wire promise on every attach span: exactly the keys `op` and
    /// `ms` with an unsigned whole `ms`, on the terminal's debug line.
    #[track_caller]
    fn assert_span_wire(lines: &[serde_json::Value], op: &str) {
        assert_eq!(lines.len(), 1, "one {op} line: {lines:?}");
        let line = &lines[0];
        assert_eq!(line["code"], "op_timing", "{line}");
        assert_eq!(line["level"], "debug", "{line}");
        assert_eq!(line["process"], "tui", "{line}");
        assert!(line.get("session_id").is_none(), "{line}");
        assert_eq!(line["message"], "An operation ended.", "{line}");
        let data = line["data"].as_object().expect("data is an object");
        let mut keys: Vec<&str> = data.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["ms", "op"], "{line}");
        assert!(line["data"]["ms"].as_u64().is_some(), "{line}");
        assert_eq!(line["data"]["op"], op, "{line}");
    }

    /// An app launched to resume the session, on home: the order every
    /// home test already builds.
    fn launch_app() -> crate::app::App {
        let mut app = crate::app::App::new(std::path::PathBuf::from("/w"));
        app.set_home(crate::home::Launch {
            location: crate::LaunchLocation::Local {
                workspace: std::path::PathBuf::from("/w"),
                project: "-w".to_owned(),
                git: false,
            },
            hover: true,
            version: "0.0.1".to_owned(),
            model: None,
            thinking: None,
            logo_glyph: "⌇".to_owned(),
            keys: crate::KeysSetup::default(),
            rail_share: 15.0,
            panel_share: 21.0,
            panel_cards: Vec::new(),
            open_at: crate::OpenAt::Session(SessionId(SESSION.to_owned()), None),
            ..Default::default()
        });
        app.set_size(60, 12);
        app
    }

    /// Drives `app` through its summary acceptance to the due tail read:
    /// the `hub_hello` gates the summary subscribe, and its acceptance
    /// queues the commands line and leaves the open due.
    fn drive_due(app: &mut crate::app::App) {
        let lines = app.on_line_inner(crate::app::testkit::hello());
        let summary = lines
            .iter()
            .find_map(|line| {
                let parsed: serde_json::Value = serde_json::from_str(line).ok()?;
                if parsed["command"] != "subscribe" {
                    return None;
                }
                parsed["id"].as_str().map(str::to_owned)
            })
            .unwrap_or_else(|| panic!("a summary subscribe among {lines:?}"));
        let accepted = contract::Envelope {
            kind: "command_accepted".to_owned(),
            session_id: SessionId(SESSION.to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: None,
            seq: None,
            payload: serde_json::json!({"command_id": summary})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        };
        let sent = app.on_line_inner(Line::Session(accepted));
        assert_eq!(sent.len(), 1, "the acceptance queues one line: {sent:?}");
        let commands: serde_json::Value = serde_json::from_str(&sent[0])
            .unwrap_or_else(|err| panic!("the queued line parses: {err}"));
        assert_eq!(commands["command"], "commands", "{commands}");
        assert_eq!(
            app.opening_session(),
            Some(SessionId(SESSION.to_owned())),
            "the open is due"
        );
    }

    /// A hub `stream_reopened` answer, which abandons the waited read
    /// without failing it.
    fn reopened() -> Input {
        Input::Hub(Line::Hub(contract::HubLine {
            kind: "stream_reopened".to_owned(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            payload: serde_json::json!({"session_id": SESSION})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        }))
    }

    /// A loop on the stepping diagnostic writer, with the `run_with`
    /// prefix simulated: setup stamped at entry, wait running after the
    /// initial draw.
    fn attach_loop(home: &fakes::TempDir, app: crate::app::App) -> super::Loop<TestBackend> {
        let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
        lp.app = app;
        lp.timing = crate::timing::Timing::new(debug_diag(home.path()));
        lp.timing.start_attach_setup();
        lp.timing.stop_attach_setup();
        lp.timing.start_attach_wait();
        lp.attached_frame = lp.timing.attached_frame();
        lp
    }

    /// Without a due tail the frame draws outside every span, and the
    /// teardown discard writes none: a home launch leaves no span line.
    #[test]
    fn attach_spans_write_nothing_without_a_due_tail() {
        let home = fakes::TempDir::new("event-loop-attach-spans-home");
        let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
        lp.timing = crate::timing::Timing::new(debug_diag(home.path()));
        lp.timing.start_attach_setup();
        lp.timing.stop_attach_setup();
        lp.timing.start_attach_wait();
        lp.attached_frame = lp.timing.attached_frame();
        let (_tx, rx) = mpsc::channel();
        assert_eq!(lp.frame_inner(&rx), None);
        assert!(op_lines(home.path()).is_empty());
        lp.timing.discard_attach_spans();
        crate::timing::discard(lp.attached_frame.take());
        assert!(op_lines(home.path()).is_empty());
    }

    /// A due attempt with no stream runs the install yet returns false:
    /// the install line stands, the five spans stay retained, and the
    /// teardown discard writes none of them.
    #[test]
    fn attach_spans_stay_held_through_a_streamless_due_attempt() {
        let home = fakes::TempDir::new("event-loop-attach-spans-no-stream");
        let mut app = launch_app();
        drive_due(&mut app);
        let mut lp = attach_loop(&home, app);
        let (_tx, rx) = mpsc::channel();
        assert!(!lp.open_tail(&rx), "no stream returns false");
        let lines = op_lines(home.path());
        let installs: Vec<serde_json::Value> = lines
            .iter()
            .filter(|line| line["data"]["op"] == "tui.tail_install")
            .cloned()
            .collect();
        assert_span_wire(&installs, "tui.tail_install");
        assert!(
            lines.iter().all(|line| !line["data"]["op"]
                .as_str()
                .unwrap_or_default()
                .starts_with("tui.attach_")),
            "no attach span emits before its draw: {lines:?}"
        );
        lp.timing.discard_attach_spans();
        crate::timing::discard(lp.attached_frame.take());
        let lines = op_lines(home.path());
        assert!(
            lines.iter().all(|line| !line["data"]["op"]
                .as_str()
                .unwrap_or_default()
                .starts_with("tui.attach_")),
            "the discard writes no span: {lines:?}"
        );
    }

    /// A reopened tail read installs and draws: all five spans emit in
    /// boundary order at the attached-history draw, each covering its
    /// own step, before the parent frame line.
    #[test]
    fn attach_spans_emit_at_the_reopened_install_draw() {
        let home = fakes::TempDir::new("event-loop-attach-spans-reopened");
        let mut app = launch_app();
        drive_due(&mut app);
        let mut lp = attach_loop(&home, app);
        let (stream, _peer) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
        lp.hub.store(stream);
        let (tx, rx) = mpsc::channel();
        for _ in 0..3 {
            tx.send(reopened())
                .unwrap_or_else(|err| panic!("queue the reopening: {err}"));
        }
        // Buffered answers stay queued; any read past them fails fast
        // as lost instead of hanging the frame.
        drop(tx);
        assert_eq!(lp.frame_inner(&rx), None);
        let lines = op_lines(home.path());
        let ops: Vec<&str> = lines
            .iter()
            .map(|line| line["data"]["op"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            ops,
            [
                "tui.attach_setup",
                "tui.attach_wait",
                "tui.attach_tail",
                "tui.attach_prepare",
                "tui.attach_draw",
                "tui.attached_frame"
            ],
            "{lines:?}"
        );
        for line in &lines {
            let op = line["data"]["op"].as_str().unwrap_or_default();
            assert_span_wire(std::slice::from_ref(line), op);
        }
        for line in lines.iter().take(5) {
            assert_eq!(line["data"]["ms"], 20, "each span covers one step: {line}");
        }
    }

    /// A rejected history answer ends the read with its message yet
    /// still installs: the attempt returns true and the draw emits all
    /// five spans. The worker parks in its history wait (the socket
    /// read is the signal), the test answers the waited command, and
    /// the socket deadline bounds a missing command.
    #[test]
    fn attach_spans_emit_at_a_rejected_history_install() {
        let home = fakes::TempDir::new("event-loop-attach-spans-rejected");
        let mut app = launch_app();
        drive_due(&mut app);
        let mut lp = attach_loop(&home, app);
        let (stream, peer) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
        lp.hub.store(stream);
        peer.set_read_timeout(Some(DEADLINE))
            .unwrap_or_else(|err| panic!("arm the socket deadline: {err}"));
        let (tx, rx) = mpsc::channel();
        // The history wait reads the channel from the worker: the mutex
        // shares the receiver across the scope's threads.
        let rx = std::sync::Mutex::new(rx);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                let held = rx
                    .lock()
                    .unwrap_or_else(|err| panic!("take the channel: {err}"));
                lp.open_tail(&held)
            });
            let mut command = String::new();
            std::io::BufReader::new(&peer)
                .read_line(&mut command)
                .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the history command: {err}"));
            let history: serde_json::Value = serde_json::from_str(&command)
                .unwrap_or_else(|err| panic!("a command parses: {err}"));
            assert_eq!(history["command"], "history", "{history}");
            let id = history["id"]
                .as_str()
                .unwrap_or_else(|| panic!("an id: {history}"));
            tx.send(Input::Hub(Line::Session(contract::Envelope {
                kind: "command_rejected".to_owned(),
                session_id: SessionId(SESSION.to_owned()),
                ts: 0,
                schema_version: contract::SCHEMA_VERSION,
                turn_id: None,
                action_id: None,
                seq: None,
                payload: serde_json::json!({"command_id": id, "message": "gone"})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            })))
            .unwrap_or_else(|err| panic!("answer the waited command: {err}"));
            assert!(worker.join().unwrap(), "a rejected read still installs");
        });
        // Reads past the answered wait fail fast as lost instead of
        // hanging the frame.
        drop(tx);
        let rx = rx
            .into_inner()
            .unwrap_or_else(|err| panic!("take the channel back: {err}"));
        assert_eq!(lp.frame_inner(&rx), None);
        let lines = op_lines(home.path());
        let ops: Vec<&str> = lines
            .iter()
            .map(|line| line["data"]["op"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            ops,
            [
                "tui.attach_setup",
                "tui.attach_wait",
                "tui.attach_tail",
                "tui.attach_prepare",
                "tui.attach_draw",
                "tui.attached_frame"
            ],
            "{lines:?}"
        );
        for line in &lines {
            let op = line["data"]["op"].as_str().unwrap_or_default();
            assert_span_wire(std::slice::from_ref(line), op);
        }
    }
}
