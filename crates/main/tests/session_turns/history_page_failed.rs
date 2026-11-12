//! Binary-level test for #3761: the real event loop on a pty against a
//! fake hub whose `history` answer for the first page holds no lines.
//! Scrolling up requests that page; scrolling back down must still
//! draw, quit must still exit 0, and exactly one "Could not load
//! history" notice must show.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use serde_json::{Value, json};
use support::pty::{Grid, Reader, Shared, TITLE, Writer, open_with_pixels, write_within};
use support::{Deadline, Setup};

/// The pty size the scroll draws on.
const COLS: u16 = 160;
const ROWS: u16 = 48;
/// The jig's own session: the fake hub serves its lines from memory.
const ID: &str = "s_paging0000000000";
/// The running turn's streamed reply: the last rows hold it.
const LAST: &str = "streamed text, a few words at a time; ";
/// The load failure the scroll must surface exactly once.
const NOTICE: &str = "Could not load history";
/// Enough presses to reach the top from the bottom, whatever a page
/// holds: extra presses stop at the top.
const SCROLLS: usize = 500;
/// A synchronized frame ends here; splitting on it yields whole frames.
const FRAME_END: &[u8] = b"\x1b[?2026l";

/// What the fake hub serves: the jig's durable lines in `seq` order.
/// `last_to` names the open's last window: the `to_seq` of the first
/// window at or below 255, or none when the jig fits the tail request
/// itself. `done` fires after that window's answer is written.
struct Hub {
    lines: Vec<contract::Envelope>,
    max_seq: u64,
    last_to: Option<u64>,
    done: mpsc::Sender<()>,
}

/// The hub's `command_accepted` for `id`, in session shape when the
/// command names one, hub shape otherwise: the loop matches answers by
/// their command id.
fn accept(command: &Value, id: &str, result: Value) -> String {
    let mut line = json!({
        "kind": "command_accepted",
        "ts": 0,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {"command_id": id, "result": result},
    });
    if let (Some(session), Some(object)) = (command.get("session_id"), line.as_object_mut()) {
        object.insert("session_id".to_owned(), session.clone());
    }
    line.to_string()
}

/// Serves `theirs`: `hub_hello`, then one answer per command. The first
/// page's `history` answers with no lines; every other window answers
/// from the jig's lines. Records each command's name and window, then
/// sends on `wake`, so a wait on the driver's wake channel sees the
/// command recorded.
fn serve(theirs: UnixStream, hub: Hub, seen: Arc<Mutex<Vec<String>>>, wake: mpsc::Sender<()>) {
    let mut reader = BufReader::new(theirs.try_clone().unwrap());
    let mut writer = theirs;
    let hello = json!({
        "kind": "hub_hello",
        "ts": 0,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {},
    });
    std::io::Write::write_all(&mut writer, format!("{hello}\n").as_bytes()).unwrap();
    let mut text = String::new();
    while reader.read_line(&mut text).is_ok_and(|read| read > 0) {
        let command: Value = serde_json::from_str(&text).unwrap_or_default();
        text.clear();
        let name = command
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        let id = command
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        let args = command.get("args").cloned().unwrap_or_default();
        let from = args.get("from_seq").and_then(Value::as_u64);
        let to = args.get("to_seq").and_then(Value::as_u64);
        seen.lock().unwrap().push(format!(
            "{name} from={from:?} to={to:?} level={:?}",
            args.get("level").and_then(Value::as_str),
        ));
        // After the state it announces
        // (`docs/code-quality.md`, "What a reviewer checks").
        wake.send(()).unwrap_or(());
        let mut open_last = false;
        let result = match name.as_str() {
            "subscribe" if args.get("level").and_then(Value::as_str) == Some("full") => {
                let answer = accept(&command, &id, json!({"last_seq": hub.max_seq}));
                std::io::Write::write_all(&mut writer, format!("{answer}\n").as_bytes()).unwrap();
                for line in &hub.lines {
                    let raw = serde_json::to_string(line).unwrap();
                    std::io::Write::write_all(&mut writer, format!("{raw}\n").as_bytes()).unwrap();
                }
                continue;
            }
            "subscribe" => json!({"last_seq": hub.max_seq}),
            "history" if from == Some(0) && to.is_some() => json!({"lines": []}),
            "history" => {
                let lines: Vec<&contract::Envelope> = hub
                    .lines
                    .iter()
                    .filter(|line| {
                        line.seq.is_some_and(|seq| {
                            from.is_none_or(|first| seq.0 >= first)
                                && to.is_none_or(|last| seq.0 <= last)
                        })
                    })
                    .collect();
                let lines = if from.is_none() {
                    lines
                        .into_iter()
                        .rev()
                        .take(256)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                } else {
                    lines
                };
                // The open's last window: the tip-to-root walk ends at
                // seq 0, so this is the last answer the open needs.
                open_last = from.is_none() && to == hub.last_to;
                json!({"lines": lines})
            }
            "commands" => json!({"commands": []}),
            _ => json!({}),
        };
        let answer = accept(&command, &id, result);
        std::io::Write::write_all(&mut writer, format!("{answer}\n").as_bytes()).unwrap();
        if open_last {
            // After the state it announces
            // (`docs/code-quality.md`, "What a reviewer checks").
            let _result = hub.done.send(());
        }
    }
}

/// Whether `command` is the request for the top page: the first window,
/// with an upper bound.
fn is_top_page_request(command: &str) -> bool {
    command.starts_with("history from=Some(0) ") && !command.contains("to=None")
}

/// Whether the hub has recorded the request for the top page.
fn top_page_requested(seen: &Mutex<Vec<String>>) -> bool {
    seen.lock()
        .unwrap()
        .iter()
        .any(|command| is_top_page_request(command))
}

/// The pty the loop draws on: the master drained from the first frame,
/// the writer typing on it.
struct Driver {
    shared: Arc<Mutex<Shared>>,
    writer: Writer,
    wakes: mpsc::Receiver<()>,
    deadline: Deadline,
    _reader: Reader,
}

/// Which frames a [`Driver::wait_screen`] may match.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Frames {
    /// The live grid only.
    Live,
    /// The live grid or the frame [`Driver::retain`] kept.
    Kept,
}

impl Driver {
    /// `wake` is the sender the reader signals each chunk on and `wakes`
    /// its receiver; the test hands `serve` a clone of `wake` too, so one
    /// receiver wakes on output and on a recorded command.
    fn spawn(
        deadline: Deadline,
        wake: mpsc::Sender<()>,
        wakes: mpsc::Receiver<()>,
    ) -> (Self, fs::File) {
        let terminal = open_with_pixels(COLS, ROWS, 0, 0);
        let slave = terminal.terminal;
        let main = fs::File::from(terminal.main);
        let shared = Arc::new(Mutex::new(Shared::new(COLS, ROWS)));
        let writer: Writer = Arc::new(Mutex::new(main.try_clone().unwrap()));
        let reader = Reader::start_with_wake(
            main,
            Arc::clone(&shared),
            Arc::clone(&writer),
            deadline,
            wake,
        );
        (
            Self {
                shared,
                writer,
                wakes,
                deadline,
                _reader: reader,
            },
            slave,
        )
    }

    fn write(&self, bytes: &[u8]) {
        write_within(&self.writer, bytes, self.deadline).expect("typing on the terminal");
    }

    fn output(&self) -> Vec<u8> {
        self.shared.lock().unwrap().output()
    }

    /// Arms the harness to keep the first whole frame holding `needle`,
    /// so [`Driver::wait_screen`] still sees it after a later frame
    /// replaced it.
    fn retain(&self, needle: &str) {
        self.shared.lock().unwrap().retain(needle);
    }

    /// Whether the output at or after `from` holds `needle`.
    fn holds_since(&self, from: usize, needle: &[u8]) -> bool {
        let output = self.shared.lock().unwrap().output();
        output.len() >= from
            && output[from..]
                .windows(needle.len())
                .any(|window| window == needle)
    }

    /// Waits until the output at or after `from` holds `needle`.
    #[track_caller]
    fn wait_bytes(&self, from: usize, needle: &[u8], what: &str) {
        let wait = self.deadline.start_wait();
        loop {
            if self.holds_since(from, needle) {
                return;
            }
            if wait.recv(&self.wakes).is_err() {
                panic!("waited until the deadline for {what}");
            }
        }
    }

    /// Whether the grid no longer holds `text`.
    fn lacks(&self, text: &str) -> bool {
        !self.shared.lock().unwrap().grid().contents.contains(text)
    }

    /// Waits after a batch of keys for a frame at or after `from`, or for
    /// the hub to record the top-page request. A batch that moves nothing
    /// draws no frame (`Screen::draw` writes nothing for equal cells), so
    /// only the request can end its wait. The hub records the request when
    /// the placeholder enters the viewport, before the scroll reaches the
    /// top, so a caller still scrolling passes `until_gone`: the request
    /// ends the wait only once the grid no longer holds that text, and
    /// before then only the batch's own frame does. The wake channel
    /// carries both signals: the reader's chunks and `serve`'s recorded
    /// commands.
    #[track_caller]
    fn wait_batch(
        &self,
        from: usize,
        seen: &Mutex<Vec<String>>,
        until_gone: Option<&str>,
        what: &str,
    ) {
        let wait = self.deadline.start_wait();
        loop {
            if self.holds_since(from, FRAME_END)
                || (top_page_requested(seen) && until_gone.is_none_or(|text| self.lacks(text)))
            {
                return;
            }
            if wait.recv(&self.wakes).is_err() {
                panic!(
                    "waited until the deadline for {what}; hub saw {:?}",
                    seen.lock().unwrap()
                );
            }
        }
    }

    /// Waits until the grid matches, however many frames arrive. A wait
    /// that outlives the loop panics naming the wait and the last grid;
    /// an expiry panics with `extra`'s report, so a stall says whether
    /// the poisoned page was even requested. With `Frames::Kept` the
    /// frame [`Driver::retain`] kept matches too; only the wait for the
    /// retained needle passes it, since any other wait must read the live
    /// grid and the kept frame is older.
    #[track_caller]
    fn wait_screen(
        &self,
        frames: Frames,
        what: &str,
        mut done: impl FnMut(&Grid) -> bool,
        extra: impl FnOnce() -> String,
    ) -> Grid {
        let wait = self.deadline.start_wait();
        let start_left = wait.left();
        loop {
            if frames == Frames::Kept {
                let retained = { self.shared.lock().unwrap().retained() };
                if let Some(kept) = retained
                    && done(&kept)
                {
                    return kept;
                }
            }
            let grid = { self.shared.lock().unwrap().grid() };
            if done(&grid) {
                return grid;
            }
            if wait.recv(&self.wakes).is_err() {
                panic!(
                    "waited until the deadline for {what} (deadline had {start_left:?} left when the wait began); screen:\n{}\n{}",
                    grid.contents,
                    extra(),
                );
            }
        }
    }
}

/// How many rows hold `needle`.
fn occurrences(grid: &Grid, needle: &str) -> usize {
    grid.rows.iter().filter(|row| row.contains(needle)).count()
}

/// The failure-path decider for the next sighting of #3883: polls the
/// loop's exit channel once and scans the raw output for the notice
/// bytes, so the panic names which surviving cause the state implies.
/// It runs only after the notice wait has already failed, so a green run
/// never calls it; the wait's needle and deadline are unchanged.
fn notice_wait_report<T: std::fmt::Debug>(
    seen: &Arc<Mutex<Vec<String>>>,
    finished: &mpsc::Receiver<T>,
    raw: &[u8],
) -> String {
    let seen = seen.lock().unwrap();
    let poisoned = seen.iter().any(|command| is_top_page_request(command));
    let base = format!("poisoned page requested: {poisoned}; hub saw: {seen:?}");
    let drew = raw
        .windows(NOTICE.len())
        .any(|window| window == NOTICE.as_bytes());
    match finished.try_recv() {
        Ok(code) => format!(
            "{base}; the loop had exited with {code:?}: the event loop exited \
             before the notice could draw (H4, early-exit path)"
        ),
        Err(mpsc::TryRecvError::Empty) if drew => format!(
            "{base}; the loop is still running and the notice bytes are in the \
             raw output but no frame holds them: retention was not armed \
             before the first scroll key, or the clock passed to the loop \
             was not the frozen one (H2)"
        ),
        Err(mpsc::TryRecvError::Empty) => format!(
            "{base}; the loop is still running and no notice bytes are in the \
             raw output: the notice never drew, so the loop thread stalled \
             (H4, check CI stderr)"
        ),
        Err(mpsc::TryRecvError::Disconnected) => format!(
            "{base}; the loop's exit channel disconnected: the loop thread died \
             without sending its exit code (H4, check CI stderr)"
        ),
    }
}

#[test]
fn scrolling_past_a_page_that_cannot_load_still_draws_and_quits() {
    let setup = Setup::new();
    let mut lines: Vec<contract::Envelope> = tui::paging_session(0)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert!(
        lines.len() > 200,
        "the jig holds pages past the first: {}",
        lines.len()
    );
    let max_seq = lines
        .iter()
        .filter_map(|line: &contract::Envelope| line.seq.map(|seq| seq.0))
        .max()
        .unwrap();
    // End the jig's still-running turn, so the session idles: a working
    // session spins the working line and asks on quit instead of
    // quitting, which is correct behavior but not what this test drives.
    let end_turn: contract::Envelope = serde_json::from_value(json!({
        "kind": "turn_completed",
        "session_id": ID,
        "ts": 1_790_604_120_000_u64 + max_seq + 1,
        "schema_version": contract::SCHEMA_VERSION,
        "payload": {"outcome": "completed"},
        "seq": max_seq + 1,
    }))
    .unwrap();
    lines.push(end_turn);
    let max_seq = max_seq + 1;
    let (ours, theirs) = UnixStream::pair().unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    // The open's last window: durable seqs run 0..=max_seq, and the
    // tip-to-root walk steps back in at most 256-line windows, so the
    // last one is the first with to_seq at or below 255; a log of 256
    // durable lines or fewer ends with the tail request itself.
    let last_to = if max_seq <= 255 {
        None
    } else {
        Some((max_seq - 256) % 256)
    };
    let (open_done, open_done_rx) = mpsc::channel();
    // One receiver wakes the driver's waits on output (the reader) and on
    // a recorded hub command (`serve`).
    let (wake, wakes) = mpsc::channel();
    let hub = Hub {
        lines,
        max_seq,
        last_to,
        done: open_done,
    };
    let served = Arc::clone(&seen);
    let served_wake = wake.clone();
    thread::spawn(move || serve(theirs, hub, served, served_wake));
    let connect: tui::Connect = Box::new(move || {
        let mut reader = BufReader::new(ours.try_clone()?);
        let mut text = String::new();
        reader.read_line(&mut text)?;
        let hello: contract::HubLine = serde_json::from_str(&text)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        Ok((ours.try_clone()?, hello))
    });
    let launch = tui::Launch {
        location: tui::LaunchLocation::Local {
            workspace: setup.workspace().to_path_buf(),
            project: "test".to_owned(),
            git: false,
        },
        open_at: tui::OpenAt::Session(contract::SessionId(ID.to_owned()), None),
        ..Default::default()
    };
    // Frozen for the whole test: the failure notice clears after 10 s on
    // the loop's clock, so a clock that passes could clear it before the
    // test looks. Every wait below keeps its wall-clock deadline.
    let clock: Arc<dyn contract::clock::Clock> =
        fakes::clock::FakeClock::with_sleep(::support::clock::Sleep::Waits);
    let diag = Arc::new(log::diag::Diag::new(
        &setup.home(),
        log::diag::Process::Tui,
        log::diag::Level::Info,
        Arc::clone(&clock),
    ));
    let (driver, tty) = Driver::spawn(setup.deadline, wake, wakes);
    let on_attach: tui::OnAttach = Box::new(|_: &contract::SessionId| {});
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let code = tui::run(tty, launch, connect, on_attach, clock, diag, None);
        done.send(code).unwrap();
    });
    // The end of the first frame proves the input reader runs before
    // any typing goes out.
    driver.wait_bytes(0, TITLE, "the end of the first frame");
    driver.wait_screen(
        Frames::Live,
        "the last turn's rows",
        |grid| grid.contents.contains(LAST),
        || format!("hub saw: {:?}", seen.lock().unwrap()),
    );
    // The open fetches the whole log tip-to-root and the fake hub
    // signals after writing the last window's answer, so the scroll
    // below starts only once the open holds every line: one receive
    // through the held deadline (`docs/testing.md`, "Waits and timeouts").
    if setup.deadline.recv(&open_done_rx).is_err() {
        panic!(
            "waited until the deadline for the open to fetch the whole log: hub saw {:?}",
            seen.lock().unwrap(),
        );
    }
    seen.lock().unwrap().clear();
    // Keep the frame that draws the notice, whenever the waits below look:
    // only the scroll below can request the top page, so nothing earlier
    // can draw it.
    driver.retain(NOTICE);
    // To the top, through the page that cannot load: five keys per
    // drawn frame. A bulk write stalls against a full input buffer
    // (write_within blocks on the deadline), so each batch waits for
    // the frame it caused before the next goes out. Counter bounded,
    // no clock: the grid says when the top is reached.
    let mut batches = 0;
    loop {
        if driver.lacks(LAST) {
            break;
        }
        assert!(
            batches < 200,
            "the scroll to reach the top after {batches} batches of 5: hub saw {:?}",
            seen.lock().unwrap(),
        );
        let from = driver.output().len();
        for _ in 0..5 {
            driver.write(b"\x1b[5~");
        }
        batches += 1;
        driver.wait_batch(
            from,
            &seen,
            Some(LAST),
            &format!("a frame for scroll batch {batches}"),
        );
    }
    let seen_report = || {
        let poisoned = top_page_requested(&seen);
        format!(
            "poisoned page requested: {poisoned}; hub saw: {:?}",
            seen.lock().unwrap()
        )
    };
    // The scroll lands first: the last turn's rows leave the screen.
    // Only then can the loop name the top page and fetch it.
    driver.wait_screen(
        Frames::Live,
        "the scroll to reach the top",
        |grid| !grid.contents.contains(LAST),
        seen_report,
    );
    // Leaving the screen does not prove the request was sent: the scroll
    // continues until `seen` shows it (recorded before the reply), and the
    // settled-screen wait below checks the notice.
    loop {
        if top_page_requested(&seen) {
            break;
        }
        assert!(
            batches < 200,
            "the top page was never requested after {batches} batches of 5: hub saw {:?}",
            seen.lock().unwrap(),
        );
        let from = driver.output().len();
        for _ in 0..5 {
            driver.write(b"\x1b[5~");
        }
        batches += 1;
        driver.wait_batch(
            from,
            &seen,
            None,
            &format!("a frame for top-page batch {batches}"),
        );
    }
    // The open may already have asked for the top page while landing:
    // either way exactly one failure notice shows for it, and the
    // scroll below asserts the loop keeps drawing past it.
    driver.wait_screen(
        Frames::Kept,
        "the load-failure notice",
        |grid| grid.contents.contains(NOTICE),
        || notice_wait_report(&seen, &finished, &driver.output()),
    );
    // Back down past it: the last rows draw again, beside the one
    // notice, and a frame drew for the scroll.
    let scrolled_at = driver.output().len();
    for _ in 0..SCROLLS {
        driver.write(b"\x1b[6~");
    }
    let grid = driver.wait_screen(
        Frames::Live,
        "rows past the page that cannot load",
        |grid| grid.contents.contains(LAST),
        seen_report,
    );
    driver.wait_bytes(
        scrolled_at,
        FRAME_END,
        "a frame drew after the scroll past the page that cannot load",
    );
    // One notice per code, shown once per terminal run
    // (`docs/tui.md`, "Notices"): every scroll past the failed page fails
    // it again with the same code and text, so exactly one grouped row
    // shows.
    assert_eq!(
        occurrences(&grid, NOTICE),
        1,
        "exactly one load-failure notice shows: {}",
        grid.contents
    );
    // A key is still handled: quit exits 0 on the idle session.
    driver.write(b"\x03\x03");
    let quit_wait = setup.deadline.start_wait();
    let quit_wait_left = quit_wait.left();
    let code = quit_wait.recv(&finished).unwrap_or_else(|_| {
        panic!(
            "waited until the deadline for the loop to quit (deadline had {quit_wait_left:?} left when the wait began; after {batches} batches of 5)"
        )
    });
    assert_eq!(code.expect("the loop to exit cleanly"), 0);
}
