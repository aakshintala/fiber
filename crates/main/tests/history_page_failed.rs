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

mod support;

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
struct Hub {
    lines: Vec<contract::Envelope>,
    max_seq: u64,
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
/// from the jig's lines. Records each command's name and window.
fn serve(theirs: UnixStream, hub: Hub, seen: Arc<Mutex<Vec<String>>>) {
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
                json!({"lines": lines})
            }
            "commands" => json!({"commands": []}),
            _ => json!({}),
        };
        let answer = accept(&command, &id, result);
        std::io::Write::write_all(&mut writer, format!("{answer}\n").as_bytes()).unwrap();
    }
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

impl Driver {
    fn spawn(deadline: Deadline) -> (Self, fs::File) {
        let terminal = open_with_pixels(COLS, ROWS, 0, 0);
        let slave = terminal.terminal;
        let main = fs::File::from(terminal.main);
        let shared = Arc::new(Mutex::new(Shared::new(COLS, ROWS)));
        let writer: Writer = Arc::new(Mutex::new(main.try_clone().unwrap()));
        let (reader, wakes) =
            Reader::start(main, Arc::clone(&shared), Arc::clone(&writer), deadline);
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

    /// Waits until the output at or after `from` holds `needle`.
    #[track_caller]
    fn wait_bytes(&self, from: usize, needle: &[u8], what: &str) {
        let wait = self.deadline.start_wait();
        loop {
            let output = self.shared.lock().unwrap().output();
            if output.len() >= from
                && output[from..]
                    .windows(needle.len())
                    .any(|window| window == needle)
            {
                return;
            }
            if wait.recv(&self.wakes).is_err() {
                panic!("waited until the deadline for {what}");
            }
        }
    }

    /// Waits until the grid matches, however many frames arrive. A wait
    /// that outlives the loop panics naming the wait and the last grid;
    /// an expiry panics with `extra`'s report, so a stall says whether
    /// the poisoned page was even requested.
    #[track_caller]
    fn wait_screen(
        &self,
        what: &str,
        mut done: impl FnMut(&Grid) -> bool,
        extra: impl FnOnce() -> String,
    ) -> Grid {
        let wait = self.deadline.start_wait();
        let start_left = wait.left();
        loop {
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
    let hub = Hub { lines, max_seq };
    let served = Arc::clone(&seen);
    thread::spawn(move || serve(theirs, hub, served));
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
    let clock: Arc<dyn contract::clock::Clock> = Arc::new(fakes::clock::SystemClock);
    let diag = Arc::new(log::diag::Diag::new(
        &setup.home(),
        log::diag::Process::Tui,
        log::diag::Level::Info,
        Arc::clone(&clock),
    ));
    let (driver, tty) = Driver::spawn(setup.deadline);
    let on_attach: tui::OnAttach = Box::new(|_: &contract::SessionId| {});
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let code = tui::run(tty, launch, connect, on_attach, clock, diag);
        done.send(code).unwrap();
    });
    // The end of the first frame proves the input reader runs before
    // any typing goes out.
    driver.wait_bytes(0, TITLE, "the end of the first frame");
    driver.wait_screen(
        "the last turn's rows",
        |grid| grid.contents.contains(LAST),
        || format!("hub saw: {:?}", seen.lock().unwrap()),
    );
    // The open fetches the whole log; wait for it to settle (no new hub
    // commands for a breath) before scrolling, so open traffic cannot
    // satisfy the scroll's waits for it.
    let settled = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let before = seen.lock().unwrap().len();
        std::thread::sleep(std::time::Duration::from_millis(200));
        if seen.lock().unwrap().len() == before && std::time::Instant::now() >= settled {
            break;
        }
        assert!(
            !driver.deadline.left().is_zero(),
            "the open to settle: hub saw {:?}",
            seen.lock().unwrap(),
        );
    }
    seen.lock().unwrap().clear();
    // To the top, through the page that cannot load: a frame draws for
    // the scroll, with the failure's notice.
    for _ in 0..SCROLLS {
        driver.write(b"\x1b[5~");
    }
    let seen_report = || {
        let seen = seen.lock().unwrap();
        let poisoned = seen.iter().any(|command| {
            command.starts_with("history from=Some(0) ") && !command.contains("to=None")
        });
        format!("poisoned page requested: {poisoned}; hub saw: {seen:?}")
    };
    // The scroll lands first: the last turn's rows leave the screen.
    // Only then can the loop name the top page and fetch it.
    driver.wait_screen(
        "the scroll to reach the top",
        |grid| !grid.contents.contains(LAST),
        seen_report,
    );
    // The open may already have asked for the top page while landing:
    // either way exactly one failure notice shows for it, and the
    // scroll below asserts the loop keeps drawing past it.
    driver.wait_screen(
        "the load-failure notice",
        |grid| grid.contents.contains(NOTICE),
        seen_report,
    );
    // Back down past it: the last rows draw again, beside the one
    // notice, and a frame drew for the scroll.
    let scrolled_at = driver.output().len();
    for _ in 0..SCROLLS {
        driver.write(b"\x1b[6~");
    }
    let grid = driver.wait_screen(
        "rows past the page that cannot load",
        |grid| grid.contents.contains(LAST),
        seen_report,
    );
    driver.wait_bytes(
        scrolled_at,
        FRAME_END,
        "a frame drew after the scroll past the page that cannot load",
    );
    assert_eq!(
        occurrences(&grid, NOTICE),
        1,
        "exactly one load-failure notice shows: {}",
        grid.contents
    );
    // A key is still handled: quit exits 0 on the idle session.
    driver.write(b"\x03\x03");
    let code = setup.deadline.recv(&finished).expect("the loop to quit");
    assert_eq!(code.expect("the loop to exit cleanly"), 0);
}
