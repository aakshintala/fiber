//! Tests for paging inside the frame: the loop fetches the pages a frame
//! needs with `history` before it draws (`docs/tui.md`, "History and
//! paging").

use super::{Input, Loop, Screen};
use crate::app::App;
use crate::link::Line;
use contract::Envelope;
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// PageUp, as the terminal sends it.
const PAGE_UP: &[u8] = b"\x1b[5~";

/// One line of the session.
fn line(kind: &str, seq: Option<u64>, action: Option<&str>, payload: Value) -> Envelope {
    Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: seq.map(contract::Seq),
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// A session whose first page holds 513 lines, three `history` answers,
/// the last of one line: a turn of 102 tool steps, then six turns of long
/// replies that push it out of the window.
fn session() -> Vec<Envelope> {
    let mut lines = Vec::new();
    let mut seq = 0u64;
    let mut push = |kind: &str, action: Option<&str>, payload: Value| {
        lines.push(line(kind, Some(seq), action, payload));
        seq += 1;
    };
    let prompt = |text: &str| json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": text}]}]});
    push("turn_started", None, prompt("first"));
    for step in 0..102 {
        let call = format!("a_t{step}");
        push("step_started", None, json!({}));
        push("assistant_message_started", Some("a_m"), json!({}));
        push(
            "tool_call_requested",
            Some(&call),
            json!({"name": "read", "arguments": {}}),
        );
        push(
            "assistant_message_completed",
            Some("a_m"),
            json!({"outcome": "completed"}),
        );
        push(
            "tool_call_completed",
            Some(&call),
            json!({"status": "completed", "content": []}),
        );
    }
    push("usage_recorded", None, json!({}));
    push("usage_recorded", None, json!({}));
    for turn in 1..7 {
        if turn > 1 {
            push("turn_started", None, prompt(&format!("turn {turn}")));
        }
        for step in 0..20 {
            let message = format!("a_r{turn}_{step}");
            push("step_started", None, json!({}));
            push("assistant_message_started", Some(&message), json!({}));
            let text = format!("reply {turn}.{step} ").repeat(12);
            push("text_completed", Some(&message), json!({"text": text}));
        }
        push("turn_completed", None, json!({"outcome": "completed"}));
    }
    lines
}

/// The durable lines from `from` to `to`, at most 256, as `history`
/// answers.
fn history(lines: &[Envelope], from: u64, to: u64) -> Vec<Envelope> {
    lines
        .iter()
        .filter(|line| line.seq.is_some_and(|seq| from <= seq.0 && seq.0 <= to))
        .take(256)
        .cloned()
        .collect()
}

/// A session line answering command `id`.
fn answer(kind: &str, payload: Value) -> Input {
    Input::Hub(Line::Session(line(kind, None, None, payload)))
}

/// What the fake hub does with each `history` command: the command and
/// the inputs to send back.
type Script = Box<dyn FnMut(&Value) -> Vec<Input> + Send>;

/// A loop at 60x12, attached to the session, with its lines folded and
/// the hub connected: the loop, the hub's end, and the inputs' sender.
fn opened() -> (
    Loop<TestBackend>,
    UnixStream,
    Sender<Input>,
    mpsc::Receiver<Input>,
) {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    app.on_line(Line::Hub(hello));
    app.attach(contract::SessionId(SESSION.to_owned()));
    for envelope in session() {
        app.on_line(Line::Session(envelope));
    }
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let screen =
        Screen::new(TestBackend::new(60, 12), 60, 12).unwrap_or_else(|err| panic!("screen: {err}"));
    let lp = Loop {
        app,
        parser: crate::keys::Parser::default(),
        screen,
        hub: Some(ours),
        tty: None,
        on_attach: Box::new(|_| {}),
        clock: fakes::clock::FakeClock::new(),
        wakeups: 0,
        stash: std::collections::VecDeque::new(),
        files_out: None,
        search: None,
        reader: None,
        model_reader: crate::catalogue::Reader::new(None),
        paste_reader: None,
        pointer: crate::mouse::Pointer::default(),
        hover: true,
        var: Box::new(|_| None),
        copy_command: None,
        open_command: None,
        title: crate::osc::Title::default(),
        save: None,
        shape: crate::osc::Shape::default(),
        retry: None,
        tick: crate::tick::TickThread::idle(),
    };
    let (tx, rx) = mpsc::channel();
    (lp, theirs, tx, rx)
}

/// Serves `history` on the hub's end with `script` on its own thread,
/// recording each command, until the loop hangs up.
fn serve(theirs: UnixStream, tx: Sender<Input>, mut script: Script) -> Arc<Mutex<Vec<Value>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    std::thread::Builder::new()
        .name("loop-hub".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(theirs);
            let mut text = String::new();
            while reader.read_line(&mut text).is_ok_and(|read| read > 0) {
                let command: Value = serde_json::from_str(&text).unwrap_or_default();
                text.clear();
                if let Ok(mut held) = record.lock() {
                    held.push(command.clone());
                }
                for input in script(&command) {
                    if tx.send(input).is_err() {
                        return;
                    }
                }
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    seen
}

/// Answers every `history` command from the session's lines.
fn answers_all() -> Script {
    let lines = session();
    Box::new(move |command| {
        let from = command["args"]["from_seq"].as_u64().unwrap_or(0);
        let to = command["args"]["to_seq"].as_u64().unwrap_or(u64::MAX);
        vec![answer(
            "command_accepted",
            json!({"command_id": command["id"], "result": {"lines": history(&lines, from, to)}}),
        )]
    })
}

/// Runs the loop on a thread over `inputs` then a double Ctrl+C, and hands
/// it back once it quits, waiting at most [`DEADLINE`].
fn run(
    mut lp: Loop<TestBackend>,
    rx: mpsc::Receiver<Input>,
    tx: &Sender<Input>,
    inputs: Vec<Input>,
) -> Loop<TestBackend> {
    for input in inputs {
        tx.send(input).unwrap_or_else(|err| panic!("send: {err}"));
    }
    tx.send(Input::Bytes(vec![0x03, 0x03]))
        .unwrap_or_else(|err| panic!("send: {err}"));
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-run".to_owned())
        .spawn(move || {
            let code = lp.run(&rx);
            done.send((code, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, lp) = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the loop to quit: {err}"));
    assert_eq!(code, 0);
    lp
}

/// The screen the loop drew last.
fn shown(lp: &Loop<TestBackend>) -> String {
    crate::view::text(lp.screen.backend().buffer())
}

/// Enough PageUps to reach the top.
fn to_the_top() -> Vec<Input> {
    (0..80).map(|_| Input::Bytes(PAGE_UP.to_vec())).collect()
}

/// The `(from_seq, to_seq)` of each history command seen.
fn ranges(seen: &Arc<Mutex<Vec<Value>>>) -> Vec<(u64, u64)> {
    seen.lock()
        .map(|held| {
            held.iter()
                .filter(|command| command["command"] == "history")
                .map(|command| {
                    (
                        command["args"]["from_seq"].as_u64().unwrap_or(u64::MAX),
                        command["args"]["to_seq"].as_u64().unwrap_or(u64::MAX),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_frame_fetches_a_dropped_page_in_answers_of_256_and_draws_it() {
    let (lp, theirs, tx, rx) = opened();
    assert!(!lp.app.pages().part(0).is_some());
    let seen = serve(theirs, tx.clone(), answers_all());
    let lp = run(lp, rx, &tx, to_the_top());
    let ranges = ranges(&seen);
    // The first page, lines 0 to 512, in three commands split at 256.
    assert!(ranges.contains(&(0, 255)), "{ranges:?}");
    assert!(ranges.contains(&(256, 511)), "{ranges:?}");
    assert!(ranges.contains(&(512, 512)), "{ranges:?}");
    for (from, to) in &ranges {
        assert!(to - from < 256, "{from}..={to}");
    }
    let screen = shown(&lp);
    let rows: Vec<&str> = screen.lines().collect();
    // To the top: the first turn's bubble starts the screen, its text
    // on the next row.
    assert!(
        rows.first()
            .is_some_and(|row| row.trim_start().starts_with('▄')),
        "{screen}"
    );
    assert!(
        rows.get(1).is_some_and(|row| row.contains("first")),
        "{screen}"
    );
    assert_eq!(lp.app.scroll().0, 0);
    assert!(lp.app.pages().part(0).is_some());
    assert_eq!(lp.app.notice(), None);
    for command in seen.lock().map(|held| held.clone()).unwrap_or_default() {
        assert_eq!(command["session_id"], SESSION);
    }
}

#[test]
fn inputs_during_the_wait_are_handled_after_the_frame_in_order() {
    let (mut lp, theirs, tx, rx) = opened();
    let mut serve_all = answers_all();
    let mut first = true;
    let script: Script = Box::new(move |command| {
        let mut inputs = Vec::new();
        if first {
            first = false;
            // Typed while the frame waits, and an answer to another
            // command.
            inputs.push(Input::Bytes(b"a".to_vec()));
            inputs.push(answer(
                "command_accepted",
                json!({"command_id": "c_other", "result": {"lines": []}}),
            ));
            inputs.push(Input::Resize);
            inputs.push(Input::Bytes(b"b".to_vec()));
        }
        inputs.extend(serve_all(command));
        inputs
    });
    let seen = serve(theirs, tx, script);
    lp.app.jump(0);
    // One frame, on a thread: it blocks on its fetches.
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("loop-step".to_owned())
        .spawn(move || {
            let code = lp.step(Input::Bytes(Vec::new()), &rx);
            done.send((code, lp, rx)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, mut lp, rx) = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the frame: {err}"));
    assert_eq!(code, None);
    assert!(!ranges(&seen).is_empty());
    assert!(lp.app.pages().part(0).is_some());
    // Held in arrival order, not yet handled.
    assert_eq!(lp.app.draft(), "");
    let held: Vec<String> = lp
        .stash
        .iter()
        .map(|input| match input {
            Input::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            Input::Hub(Line::Session(line)) => line.kind.clone(),
            Input::Hub(Line::Hub(_))
            | Input::Connected(..)
            | Input::ConnectFailed(_)
            | Input::Disconnected
            | Input::FindDue(_)
            | Input::Tick
            | Input::Files { .. }
            | Input::Models(_)
            | Input::Image { .. } => "other".to_owned(),
            Input::Resize => "resize".to_owned(),
        })
        .collect();
    assert_eq!(held, ["a", "command_accepted", "resize", "b"]);
    while let Some(input) = lp.stash.pop_front() {
        assert_eq!(lp.step(input, &rx), None);
    }
    assert_eq!(lp.app.draft(), "ab");
    assert_eq!(lp.app.notice(), None);
}

#[test]
fn a_lost_connection_ends_the_wait_with_the_notice() {
    let (lp, theirs, tx, rx) = opened();
    let script: Script = Box::new(|_| vec![Input::Disconnected]);
    let seen = serve(theirs, tx.clone(), script);
    let lp = run(lp, rx, &tx, to_the_top());
    assert_eq!(ranges(&seen).len(), 1);
    assert!(lp.hub.is_none());
    assert_eq!(
        lp.app.notice(),
        Some("Could not load history: connection lost")
    );
    assert_eq!(lp.app.scroll().0, 0);
    assert!(!lp.app.pages().part(0).is_some());
    // The rows stay, blank.
    assert!(!shown(&lp).contains("first"));
}

#[test]
fn a_drop_during_a_history_fetch_gives_one_retry() {
    let (mut lp, theirs, tx, rx) = opened();
    let retry = crate::retry::Retry::new(&lp.clock);
    lp.retry = Some(Arc::clone(&retry));
    let script: Script = Box::new(|_| vec![Input::Disconnected]);
    serve(theirs, tx.clone(), script);
    let lp = run(lp, rx, &tx, to_the_top());
    // One drop, one permit: the first delay.
    assert_eq!(retry.held(), Some(Duration::from_millis(500)));
    assert_eq!(
        lp.app.banner().as_deref(),
        Some("Connection lost · reconnecting (attempt 1)…")
    );
}

#[test]
fn a_rejected_fetch_sets_the_notice_and_the_frame_goes_on() {
    let (lp, theirs, tx, rx) = opened();
    let mut serve_all = answers_all();
    let script: Script = Box::new(move |command| {
        if command["args"]["from_seq"] == 0 {
            vec![answer(
                "command_rejected",
                json!({"command_id": command["id"], "code": "invalid_arguments", "message": "from_seq is past the latest line"}),
            )]
        } else {
            serve_all(command)
        }
    });
    let seen = serve(theirs, tx.clone(), script);
    let lp = run(lp, rx, &tx, to_the_top());
    assert_eq!(
        lp.app.notice(),
        Some("Could not load history: from_seq is past the latest line")
    );
    // The failed page is asked for once; the other pages still load.
    let ranges = ranges(&seen);
    assert_eq!(ranges.iter().filter(|(from, _)| *from == 0).count(), 1);
    assert!(ranges.iter().any(|(from, _)| *from > 512), "{ranges:?}");
    assert!(!lp.app.pages().part(0).is_some());
    assert!(lp.hub.is_some());
}

#[test]
fn a_frame_with_no_hub_fetches_nothing() {
    let (mut lp, theirs, tx, rx) = opened();
    drop(theirs);
    lp.hang_up();
    let lp = run(lp, rx, &tx, to_the_top());
    assert!(!lp.app.pages().part(0).is_some());
    assert_eq!(lp.app.scroll().0, 0);
}

#[test]
fn an_answer_with_no_lines_fails_the_page_once() {
    let (lp, theirs, tx, rx) = opened();
    let script: Script = Box::new(|command| {
        vec![answer(
            "command_accepted",
            json!({"command_id": command["id"], "result": {"lines": []}}),
        )]
    });
    let seen = serve(theirs, tx.clone(), script);
    let lp = run(lp, rx, &tx, to_the_top());
    assert_eq!(
        lp.app.notice(),
        Some("Could not load history: the answer held none of its lines")
    );
    // Each page is asked for once.
    let ranges = ranges(&seen);
    let mut froms: Vec<u64> = ranges.iter().map(|(from, _)| *from).collect();
    froms.dedup();
    assert_eq!(froms.len(), ranges.len(), "{ranges:?}");
    assert!(!lp.app.pages().part(0).is_some());
}

#[test]
fn a_history_command_that_cannot_be_written_loses_the_connection() {
    let (lp, _theirs, tx, rx) = opened();
    if let Some(hub) = &lp.hub {
        hub.shutdown(std::net::Shutdown::Write)
            .unwrap_or_else(|err| panic!("shutdown: {err}"));
    }
    let lp = run(lp, rx, &tx, to_the_top());
    assert!(lp.hub.is_none());
    assert_eq!(
        lp.app.notice(),
        Some("Could not load history: connection lost")
    );
    assert!(!lp.app.pages().part(0).is_some());
}

#[test]
fn the_paging_jig_opens_the_ended_turns_and_appends_the_running_one() {
    let mut lines = session();
    let next = lines.len() as u64;
    lines.push(line(
        "turn_started",
        Some(next),
        None,
        json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": "more"}]}]}),
    ));
    lines.push(line(
        "assistant_message_delta",
        None,
        Some("a_live"),
        json!({"text": "streaming"}),
    ));
    let events: String = lines
        .iter()
        .map(|line| serde_json::to_string(line).unwrap_or_default() + "\n")
        .collect();
    let report = crate::measure_paging(&events, 60, 12, fakes::clock::FakeClock::new());
    let report = report.unwrap_or_else(|error| panic!("{error}"));
    assert!(
        report.starts_with(&format!("lines: {}\n", next + 1)),
        "{report}"
    );
    assert!(report.contains("turns: 6\n"), "{report}");
    assert!(report.contains("calls: 102\n"), "{report}");
    assert!(
        report.contains("slowest append frame: 0.00 ms, of 2;"),
        "{report}"
    );
    assert!(report.contains(", of 6 paging up"), "{report}");
    // An unreadable line names its number.
    let error = crate::measure_paging("{\n", 60, 12, fakes::clock::FakeClock::new());
    assert!(error.is_err_and(|error| error.starts_with("line 1:")));
}

/// `lines` as the paging jig reads them, one envelope per line.
fn events(lines: &[Envelope]) -> String {
    lines
        .iter()
        .map(|line| serde_json::to_string(line).unwrap_or_default() + "\n")
        .collect()
}

#[test]
fn the_paging_report_counts_every_loaded_line() {
    let report = crate::measure_paging(&events(&session()), 60, 12, fakes::clock::FakeClock::new())
        .unwrap_or_else(|error| panic!("{error}"));
    // Every page loaded while paging up keeps its first line: dropping one
    // draws fewer rows. Six turns draw one time row, two bubble edges and
    // two card edges each.
    assert!(report.contains("rows: 343\n"), "{report}");
}

#[test]
fn the_paging_report_counts_the_search_matches_across_the_log() {
    let mut lines = session();
    // "shell" in the first turn and the last: the first turn is dropped by
    // the time the search runs, so its match comes from a `history` fetch.
    for line in &mut lines {
        let action = line.action_id.as_ref().map(|id| id.0.as_str());
        let text = match action {
            Some("a_r1_0") => Some("the shell here"),
            Some("a_r6_0") => Some("shell and shell"),
            _ => None,
        };
        if let (Some(text), "text_completed") = (text, line.kind.as_str()) {
            line.payload.insert("text".to_owned(), json!(text));
        }
    }
    let report = crate::measure_paging(&events(&lines), 60, 12, fakes::clock::FakeClock::new())
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(report.contains(", 3 matches\n"), "{report}");
}

/// A clock that ticks one millisecond per `now()`, so the paging jig's
/// report holds nonzero durations. The origin is the fake clock's, since
/// reading the process clock is banned in tests.
struct TickClock {
    origin: std::time::Instant,
    ticks: AtomicU64,
}

impl TickClock {
    fn clock(origin: std::time::Instant) -> Arc<Self> {
        Arc::new(Self {
            origin,
            ticks: AtomicU64::new(0),
        })
    }
}

impl contract::clock::Clock for TickClock {
    fn now(&self) -> std::time::Instant {
        let ticks = self.ticks.fetch_add(1, Ordering::SeqCst);
        self.origin
            .checked_add(Duration::from_millis(ticks))
            .unwrap_or(self.origin)
    }

    fn wall(&self) -> std::time::SystemTime {
        std::time::SystemTime::UNIX_EPOCH
    }

    fn sleep(&self, _d: Duration) {}

    fn wait_until(
        &self,
        _until: Option<std::time::Instant>,
        wait: &mut dyn FnMut(Option<Duration>),
    ) {
        wait(Some(Duration::ZERO));
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

#[test]
fn the_paging_report_prints_milliseconds() {
    let origin = fakes::clock::FakeClock::new().origin();
    let report = crate::measure_paging(&events(&session()), 60, 12, TickClock::clock(origin))
        .unwrap_or_else(|error| panic!("{error}"));
    let line = report
        .lines()
        .find(|line| line.starts_with("open pass and first frame: "))
        .unwrap_or_else(|| panic!("no open pass line in {report}"));
    // Seconds per frame would print here as milliseconds: with a ticking
    // clock the open pass takes whole milliseconds, never zero.
    let ms: f64 = line
        .trim_start_matches("open pass and first frame: ")
        .trim_end_matches(" ms")
        .parse()
        .unwrap_or_else(|error| panic!("{error} in {report}"));
    assert!(ms > 0.0, "{report}");
}

#[test]
fn paging_up_ends_at_the_top() {
    let events = events(&session());
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        done.send(crate::measure_paging(
            &events,
            60,
            12,
            fakes::clock::FakeClock::new(),
        ))
        .unwrap_or(());
    });
    // On a thread with a wall-clock deadline: a loop bound that never ends
    // fails here, instead of hanging the suite.
    let report = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|error| panic!("waited {DEADLINE:?} for the paging jig: {error}"));
    assert!(report.is_ok(), "{report:?}");
}

#[test]
fn the_paging_report_names_the_furthest_jump_row() {
    let report = crate::measure_paging(&events(&session()), 60, 12, fakes::clock::FakeClock::new())
        .unwrap_or_else(|error| panic!("{error}"));
    let total: usize = report
        .lines()
        .find_map(|line| line.strip_prefix("rows: "))
        .and_then(|rows| rows.parse().ok())
        .unwrap_or_else(|| panic!("no rows in {report}"));
    // "slowest jump frame: 0.00 ms, of 20 to row 297": the jumps spread
    // across the session, so the furthest is total * (jumps - 1) / jumps.
    // `%` or `*` for `/` lands within a screen or past the session.
    let jump = report
        .lines()
        .find(|line| line.starts_with("slowest jump frame: "))
        .unwrap_or_else(|| panic!("no jump line in {report}"));
    let after = jump
        .split_once(", of ")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("no jump count in {report}"));
    let (count, row) = after
        .split_once(" to row ")
        .unwrap_or_else(|| panic!("no jump row in {report}"));
    let (count, row): (usize, usize) = (
        count
            .parse()
            .unwrap_or_else(|error| panic!("{error} in {report}")),
        row.parse()
            .unwrap_or_else(|error| panic!("{error} in {report}")),
    );
    assert_eq!(row, total.saturating_mul(count - 1) / count, "{report}");
}

#[test]
fn a_selection_waiting_on_a_page_copies_in_the_same_step() {
    let (mut lp, theirs, tx, rx) = opened();
    let dir = fakes::TempDir::new("tui-select-page");
    let path = dir.path().join("tty");
    lp.tty = Some(std::fs::File::create(&path).unwrap_or_else(|err| panic!("tty: {err}")));
    let seen = serve(theirs, tx.clone(), answers_all());
    // At the top, page 0 loads; a drag over its first rows, then End, which
    // drops it, then the release: the copy waits on page 0, which the same
    // step fetches, and copies.
    let mut inputs = to_the_top();
    inputs.push(Input::Bytes(b"\x1b[<0;1;1M\x1b[<32;40;3M".to_vec()));
    inputs.push(Input::Bytes(b"\x1b[F".to_vec()));
    inputs.push(Input::Bytes(b"\x1b[<0;40;3m".to_vec()));
    let lp = run(lp, rx, &tx, inputs);
    assert_eq!(lp.app.pages().pinned(), 0, "its pages stay pinned");
    let before = ranges(&seen).iter().filter(|range| range.0 == 0).count();
    assert!(
        before >= 2,
        "page 0 was not fetched again: {:?}",
        ranges(&seen)
    );
    let written = std::fs::read(&path).unwrap_or_else(|err| panic!("read tty: {err}"));
    let text = String::from_utf8_lossy(&written).into_owned();
    let start = text.find("\x1b]52;c;").expect("an OSC 52 copy");
    let payload: String = text
        .get(start + 7..)
        .unwrap_or_default()
        .chars()
        .take_while(|ch| *ch != '\x07')
        .collect();
    use base64::Engine as _;
    let copied = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .unwrap_or_else(|err| panic!("base64: {err}"));
    let copied = String::from_utf8_lossy(&copied).into_owned();
    assert!(copied.contains("first"), "{copied:?}");
}

/// A backend wrapping `TestBackend` that counts `flush` calls: one per
/// frame written, since a frame identical to the last writes nothing.
struct CountingBackend {
    inner: TestBackend,
    flushes: std::rc::Rc<std::cell::Cell<usize>>,
}

impl ratatui::backend::Backend for CountingBackend {
    type Error = <TestBackend as ratatui::backend::Backend>::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.flushes.set(self.flushes.get().saturating_add(1));
        self.inner.flush()
    }
}

/// A loop on the counting backend at 60x12, attached to the session as
/// [`opened`] does but with no lines folded yet, and its flush count.
fn counting() -> (
    Loop<CountingBackend>,
    std::rc::Rc<std::cell::Cell<usize>>,
    Sender<Input>,
    mpsc::Receiver<Input>,
) {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    app.on_line(Line::Hub(hello));
    app.attach(contract::SessionId(SESSION.to_owned()));
    let flushes = std::rc::Rc::new(std::cell::Cell::new(0));
    let backend = CountingBackend {
        inner: TestBackend::new(60, 12),
        flushes: std::rc::Rc::clone(&flushes),
    };
    let screen = Screen::new(backend, 60, 12).unwrap_or_else(|err| panic!("screen: {err}"));
    let lp = Loop {
        app,
        parser: crate::keys::Parser::default(),
        screen,
        hub: None,
        tty: None,
        on_attach: Box::new(|_| {}),
        clock: fakes::clock::FakeClock::new(),
        wakeups: 0,
        stash: std::collections::VecDeque::new(),
        files_out: None,
        search: None,
        reader: None,
        model_reader: crate::catalogue::Reader::new(None),
        paste_reader: None,
        pointer: crate::mouse::Pointer::default(),
        hover: true,
        var: Box::new(|_| None),
        copy_command: None,
        open_command: None,
        title: crate::osc::Title::default(),
        save: None,
        shape: crate::osc::Shape::default(),
        retry: None,
        tick: crate::tick::TickThread::idle(),
    };
    let (tx, rx) = mpsc::channel();
    (lp, flushes, tx, rx)
}

#[test]
fn queued_hub_lines_draw_one_frame() {
    let (mut lp, flushes, tx, rx) = counting();
    let mut seq = 0u64;
    let mut hub = |kind: &str, action: Option<&str>, payload: Value| {
        let envelope = line(kind, Some(seq), action, payload);
        seq += 1;
        tx.send(Input::Hub(Line::Session(envelope)))
            .unwrap_or_else(|err| panic!("send: {err}"));
    };
    for (turn, marker) in ["marker one", "marker two", "marker three"]
        .into_iter()
        .enumerate()
    {
        hub(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("prompt {turn}")}]}]}),
        );
        hub(
            "text_completed",
            Some(&format!("a_m{turn}")),
            json!({"text": marker}),
        );
        hub("turn_completed", None, json!({"outcome": "completed"}));
    }
    drop(tx);
    // Every line queued before the loop runs: one run of waiting lines
    // draws one frame.
    assert_eq!(lp.run(&rx), 0);
    assert_eq!(flushes.get(), 1, "one frame for the queued lines");
    let screen = crate::view::text(lp.screen.backend().inner.buffer());
    assert!(screen.contains("marker three"), "{screen}");
    assert!(!screen.contains("esc to interrupt"), "{screen}");
}

/// Runs `lp` over `inputs` queued before it starts, to the end of input.
fn run_queued(
    lp: &mut Loop<CountingBackend>,
    rx: mpsc::Receiver<Input>,
    tx: Sender<Input>,
    inputs: Vec<Input>,
) -> i32 {
    for input in inputs {
        tx.send(input).unwrap_or_else(|err| panic!("send: {err}"));
    }
    drop(tx);
    lp.run(&rx)
}

/// One turn's lines with `marker` as its reply, numbered from `seq`.
fn turn_lines(marker: &str, turn: usize, seq: &mut u64) -> Vec<Input> {
    let mut lines = Vec::new();
    let mut push = |kind: &str, action: Option<String>, payload: Value| {
        lines.push(Input::Hub(Line::Session(line(
            kind,
            Some(*seq),
            action.as_deref(),
            payload,
        ))));
        *seq += 1;
    };
    push(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": format!("prompt {turn}")}]}]}),
    );
    push(
        "text_completed",
        Some(format!("a_m{turn}")),
        json!({"text": marker}),
    );
    push("turn_completed", None, json!({"outcome": "completed"}));
    lines
}

/// The screen a counting loop drew last.
fn counted(lp: &Loop<CountingBackend>) -> String {
    crate::view::text(lp.screen.backend().inner.buffer())
}

#[test]
fn one_batch_counts_its_open_page_once() {
    let (mut lp, _, tx, rx) = counting();
    let mut seq = 0u64;
    let mut inputs = turn_lines("marker one", 0, &mut seq);
    inputs.truncate(2);
    let before = lp.app.pages().recounts;
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    // The batch folds both lines before it counts: one recount for the
    // batch, not one per changed line.
    assert_eq!(lp.app.pages().recounts - before, 1);
    let screen = counted(&lp);
    assert!(screen.contains("marker one"), "{screen}");
}

#[test]
fn one_line_alone_counts_its_page_at_once() {
    let (mut lp, _, tx, rx) = counting();
    let mut seq = 0u64;
    let mut inputs = turn_lines("marker one", 0, &mut seq);
    inputs.truncate(1);
    let before = lp.app.pages().recounts;
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    // A batch of one counts its page once, at its end.
    assert_eq!(lp.app.pages().recounts - before, 1);
}

#[test]
fn a_batch_with_no_changed_line_counts_nothing() {
    let (mut lp, _, tx, rx) = counting();
    let mut seq = 0u64;
    let mut step = || {
        let envelope = line("step_started", Some(seq), None, json!({}));
        seq += 1;
        Input::Hub(Line::Session(envelope))
    };
    let inputs = vec![step(), step()];
    let before = lp.app.pages().recounts;
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    // Neither line changed a card, so the batch marks nothing and
    // counts nothing.
    assert_eq!(lp.app.pages().recounts - before, 0);
}

#[test]
fn lines_a_key_and_lines_draw_three_frames() {
    let (mut lp, flushes, tx, rx) = counting();
    let mut seq = 0u64;
    let mut inputs = turn_lines("marker one", 0, &mut seq);
    inputs.push(Input::Bytes(b"a".to_vec()));
    inputs.extend(turn_lines("marker two", 1, &mut seq));
    // The lines share a frame each, and the key keeps its own.
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    assert_eq!(flushes.get(), 3);
    assert_eq!(lp.app.draft(), "a");
    let screen = counted(&lp);
    assert!(screen.contains("marker two"), "{screen}");
    assert!(!screen.contains("esc to interrupt"), "{screen}");
}

#[test]
fn more_than_a_batch_of_lines_draws_two_frames() {
    let (mut lp, flushes, tx, rx) = counting();
    let full = super::batch::HUB_BATCH / 3;
    let mut seq = 0u64;
    let mut inputs = Vec::new();
    for turn in 0..full {
        inputs.extend(turn_lines(&format!("marker {turn}"), turn, &mut seq));
    }
    assert_eq!(inputs.len(), super::batch::HUB_BATCH - 1);
    inputs.extend(turn_lines(&format!("marker {full}"), full, &mut seq));
    inputs.truncate(super::batch::HUB_BATCH + 1);
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    assert_eq!(flushes.get(), 2);
    let screen = counted(&lp);
    assert!(screen.contains(&format!("marker {full}")), "{screen}");
    assert!(!screen.contains("esc to interrupt"), "{screen}");
}

#[test]
fn a_tick_among_queued_lines_draws_no_frame_of_its_own() {
    let (mut lp, flushes, tx, rx) = counting();
    let mut seq = 0u64;
    let mut inputs = turn_lines("marker one", 0, &mut seq);
    inputs.push(Input::Tick);
    inputs.extend(turn_lines("marker two", 1, &mut seq));
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    assert_eq!(flushes.get(), 1);
    let screen = counted(&lp);
    assert!(screen.contains("marker two"), "{screen}");
    assert!(!screen.contains("esc to interrupt"), "{screen}");
}

#[test]
fn a_disconnect_among_lines_ends_the_batch_with_its_own_frame() {
    let (mut lp, flushes, tx, rx) = counting();
    let (ours, _theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    let mut seq = 0u64;
    let mut inputs = vec![Input::Connected(ours, hello)];
    inputs.extend(turn_lines("marker one", 0, &mut seq));
    inputs.push(Input::Disconnected);
    inputs.extend(turn_lines("marker two", 1, &mut seq));
    assert_eq!(run_queued(&mut lp, rx, tx, inputs), 0);
    // The connect, the first turn, the disconnect and the second turn
    // each draw their own frame.
    assert_eq!(flushes.get(), 4);
    assert_eq!(lp.app.notice(), Some("Connection lost."));
    let screen = counted(&lp);
    assert!(screen.contains("marker two"), "{screen}");
}

/// One finished `attention` line naming `name`.
fn finished(name: &str) -> Input {
    Input::Hub(Line::Hub(contract::HubLine {
        kind: "attention".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({
            "session_id": "s_aaaaaaaaaaaaaaaa",
            "name": name,
            "reason": "finished",
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    }))
}

#[test]
fn two_finished_lines_in_one_batch_write_both_notifications_in_order() {
    let dir = fakes::TempDir::new("tui-attention-batch");
    let path = dir.path().join("tty");
    let tty = std::fs::File::create(&path).unwrap_or_else(|err| panic!("create: {err}"));
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(60, 12), Some(tty));
    lp.app.set_osc9(true);
    // Both lines fold into one batch, so one frame queues both alerts:
    // neither may be lost.
    assert_eq!(
        super::tests::feed(&mut lp, vec![finished("one"), finished("two")]),
        0
    );
    let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("read: {err}"));
    let one = "\x1b]9;Fiber: one finished\x07";
    let two = "\x1b]9;Fiber: two finished\x07";
    let count = |needle: &str| {
        bytes
            .windows(needle.len())
            .filter(|w| *w == needle.as_bytes())
            .count()
    };
    assert_eq!(count(one), 1);
    assert_eq!(count(two), 1);
    let at = |needle: &str| {
        bytes
            .windows(needle.len())
            .position(|w| w == needle.as_bytes())
            .unwrap_or(usize::MAX)
    };
    assert!(at(one) < at(two));
}

/// How long the loop may take to drain the replay before the test fails
/// it: a named receive deadline, never a sleep on the test thread.
const DRAIN: Duration = Duration::from_secs(5);

/// Lower-case words, the same for the seed.
fn filler(len: usize, seed: u64) -> String {
    const WORDS: [&str; 8] = [
        "turn", "tool", "call", "file", "session", "context", "model", "log",
    ];
    let mut state = seed;
    let mut text = String::with_capacity(len + 8);
    while text.len() < len {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let pick = usize::try_from(state >> 61).unwrap_or_default();
        text.push_str(WORDS.get(pick).copied().unwrap_or("x"));
        text.push(' ');
    }
    text.truncate(len);
    text
}

/// A log of `turns` handoff turns with 4 KiB replies, the last reply
/// `last`: one screenful per reply, a handoff between turns.
fn handoff_log(turns: u64, last: &str) -> Vec<Envelope> {
    let mut lines = Vec::new();
    let mut seq = 0u64;
    let mut push = |kind: &str, action: Option<&str>, payload: Value| {
        lines.push(line(kind, Some(seq), action, payload));
        seq += 1;
    };
    for turn in 0..turns {
        push(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver", "content": [{"type": "text", "text": format!("turn {turn}")}]}]}),
        );
        push("step_started", None, json!({}));
        let message = format!("a_m{turn}");
        push("assistant_message_started", Some(&message), json!({}));
        let text = if turn + 1 == turns {
            last.to_owned()
        } else {
            filler(4096, turn)
        };
        push("text_completed", Some(&message), json!({"text": text}));
        push(
            "assistant_message_completed",
            Some(&message),
            json!({"outcome": "completed"}),
        );
        push("turn_completed", None, json!({"outcome": "completed"}));
        push("usage_recorded", None, json!({}));
        if turn + 1 < turns {
            push("handoff_started", None, json!({"trigger": "manual"}));
            push(
                "handoff_completed",
                None,
                json!({"outcome": "completed", "tokens_before": 1000}),
            );
        }
    }
    lines
}

/// A batch settles once at its end: trimming mid-batch would drop pages
/// the batch's uncounted rows still hold in the window, so the head pages
/// stay resident until the end, and only the exact counts trim them.
#[test]
fn a_batch_settles_once_at_its_end() {
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    app.attach(contract::SessionId(SESSION.to_owned()));
    app.begin_batch();
    assert!(app.pages().holding());
    for envelope in handoff_log(150, "the last reply holds quokkas") {
        app.on_line(Line::Session(envelope));
    }
    assert!(app.pages().part(0).is_some());
    assert!(app.pages().part(1).is_some());
    app.end_batch();
    assert!(!app.pages().holding());
    assert!(app.pages().page_count() > 2);
    // The head leaves once the counts are exact.
    assert!(app.pages().part(0).is_none());
}

/// `fiber resume` opens through the hub: feed, recent, the subscribe and
/// the session's commands, then the whole log streams as live lines. The
/// loop folds every batch and draws the tail: the last reply shows, and
/// no page is fetched twice.
#[test]
fn resume_open_replays_the_log_to_the_tail() {
    use std::io::{BufRead, BufReader, Write};
    let lines = handoff_log(150, "the last reply holds quokkas");
    let mut app = App::new(PathBuf::from("/w"));
    app.set_home(crate::home::Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        open_at: crate::OpenAt::Session(contract::SessionId(SESSION.to_owned())),
        ..Default::default()
    });
    app.set_size(60, 12);
    let screen =
        Screen::new(TestBackend::new(60, 12), 60, 12).unwrap_or_else(|err| panic!("screen: {err}"));
    let mut lp = Loop {
        app,
        parser: crate::keys::Parser::default(),
        screen,
        hub: None,
        tty: None,
        on_attach: Box::new(|_| {}),
        clock: fakes::clock::FakeClock::new(),
        wakeups: 0,
        stash: std::collections::VecDeque::new(),
        files_out: None,
        search: None,
        reader: None,
        model_reader: crate::catalogue::Reader::new(None),
        paste_reader: None,
        pointer: crate::mouse::Pointer::default(),
        hover: true,
        var: Box::new(|_| None),
        copy_command: None,
        open_command: None,
        title: crate::osc::Title::default(),
        save: None,
        shape: crate::osc::Shape::default(),
        retry: None,
        tick: crate::tick::TickThread::idle(),
    };
    let (tx, rx) = mpsc::channel();
    let (tui_end, hub_end) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let read_end = tui_end
        .try_clone()
        .unwrap_or_else(|err| panic!("clone: {err}"));
    let read_tx = tx.clone();
    std::thread::Builder::new()
        .name("resume-read".to_owned())
        .spawn(move || crate::link::read_lines(read_end, &read_tx))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // The hub: answers commands, then streams the log on subscribe and
    // hangs up, so the loop drains and returns.
    let stream = lines.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    std::thread::Builder::new()
        .name("resume-hub".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(hub_end.try_clone().unwrap());
            let mut hub_end = hub_end;
            let mut text = String::new();
            let mut write = |value: Value| {
                let mut bytes = serde_json::to_string(&value).unwrap_or_default();
                bytes.push('\n');
                hub_end.write_all(bytes.as_bytes()).unwrap_or(());
            };
            let hub_ack = |id: &Value, result: Value| {
                json!({"kind": "command_accepted", "ts": 0,
                    "schema_version": contract::SCHEMA_VERSION,
                    "payload": {"command_id": id, "result": result}})
            };
            let session_ack = |id: &Value, result: Value| {
                let envelope = line("command_accepted", None, None, json!({}));
                let mut value = serde_json::to_value(&envelope).unwrap_or(Value::Null);
                value["payload"] = json!({"command_id": id, "result": result});
                value
            };
            while reader.read_line(&mut text).is_ok_and(|read| read > 0) {
                let command: Value = serde_json::from_str(&text).unwrap_or_default();
                text.clear();
                if let Ok(mut held) = record.lock() {
                    held.push(command.clone());
                }
                let id = command["id"].clone();
                match command["command"].as_str() {
                    Some("feed") => write(hub_ack(&id, json!({}))),
                    Some("recent") => write(hub_ack(&id, json!({"sessions": []}))),
                    Some("subscribe") => {
                        // A command naming a session is relayed, never
                        // answered by the hub itself: only the session's
                        // accept arrives, carrying its id, then the log.
                        write(session_ack(&id, json!({})));
                        for envelope in &stream {
                            write(serde_json::to_value(envelope).unwrap_or(Value::Null));
                        }
                        return;
                    }
                    Some("commands") => {
                        write(session_ack(&id, json!({"commands": []})));
                    }
                    Some("history") => {
                        let from = command["args"]["from_seq"].as_u64().unwrap_or(0);
                        let to = command["args"]["to_seq"].as_u64().unwrap_or(u64::MAX);
                        write(session_ack(
                            &id,
                            json!({"lines": history(&stream, from, to)}),
                        ));
                    }
                    _ => write(hub_ack(&id, json!({}))),
                }
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    tx.send(Input::Connected(tui_end, hello))
        .unwrap_or_else(|err| panic!("send: {err}"));
    drop(tx);
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("resume-run".to_owned())
        .spawn(move || {
            let code = lp.run(&rx);
            done.send((code, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, lp) = finished
        .recv_timeout(DRAIN)
        .unwrap_or_else(|err| panic!("waited {DRAIN:?} for the loop to drain: {err}"));
    assert_eq!(code, 0);
    assert!(ranges(&seen).is_empty(), "{:?}", ranges(&seen));
    let screen = shown(&lp);
    assert!(screen.contains("quokkas"), "{screen}");
}

/// A left click at 0-based `col`, `row`: the press and the release.
fn click(col: u16, row: u16) -> Input {
    let (col, row) = (col + 1, row + 1);
    Input::Bytes(format!("\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m").into_bytes())
}

/// Ctrl+C twice, which quits the loop with code 0.
fn quit() -> Input {
    Input::Bytes(vec![0x03, 0x03])
}

/// A wide loop with the delegates card, attached, holding one running
/// Fiber delegate: the first delegate spot's 0-based cell.
fn delegate_spot() -> (Loop<TestBackend>, (u16, u16)) {
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(160, 40), None);
    lp.app.set_size(160, 40);
    lp.screen
        .resize(160, 40)
        .unwrap_or_else(|err| panic!("resize: {err}"));
    lp.app.set_home(crate::home::Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: ["session", "changed_files", "delegates", "jobs", "quota"]
            .map(str::to_owned)
            .to_vec(),
        ..Default::default()
    });
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    let hub =
        |kind: &str, payload: Value| Input::Hub(Line::Session(line(kind, None, None, payload)));
    let code = super::tests::feed(
        &mut lp,
        vec![
            hub(
                "job_started",
                json!({"job_id": "j_1", "description": "task one", "output_path": "/tmp/out"}),
            ),
            hub(
                "delegate_started",
                json!({"job_id": "j_1", "delegate_session_id": "s_bbbbbbbbbbbbbbbb",
                    "harness": "fiber", "model": "test/model", "workspace": "/w"}),
            ),
        ],
    );
    assert_eq!(code, 0);
    assert!(!lp.app.item_open());
    // Either delegate row opens the delegate; the description names it.
    let panel = lp
        .app
        .chrome()
        .layout()
        .and_then(|layout| layout.panel)
        .unwrap_or_else(|| panic!("a panel"));
    let row = shown(&lp)
        .lines()
        .position(|text| text.contains("task one"))
        .unwrap_or_else(|| panic!("the delegate row"));
    (lp, (panel.x + 2, u16::try_from(row).unwrap_or(u16::MAX)))
}

/// A key batch never holds: opening a delegate swaps the shown screen,
/// and a hold would stick to the stashed parent past the batch's end.
/// The click opens the delegate, and the quit ends the run: neither
/// the shown nor the stashed screen is held after.
#[test]
fn a_key_that_opens_a_delegate_leaves_nothing_held() {
    let (mut lp, (col, row)) = delegate_spot();
    let (tx, rx) = mpsc::channel();
    tx.send(click(col, row))
        .unwrap_or_else(|err| panic!("send: {err}"));
    tx.send(quit()).unwrap_or_else(|err| panic!("send: {err}"));
    drop(tx);
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("delegate-run".to_owned())
        .spawn(move || {
            let code = lp.run(&rx);
            done.send((code, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, lp) = finished
        .recv_timeout(DRAIN)
        .unwrap_or_else(|err| panic!("waited {DRAIN:?} for the loop to quit: {err}"));
    assert_eq!(code, 0);
    assert!(lp.app.item_open());
    assert!(!lp.app.pages().holding());
    assert!(!lp.app.stashed_holding());
}

/// Quitting ends the run through the batch's end, so a hold never
/// outlives the loop: a quit in its own batch leaves nothing held.
#[test]
fn a_quit_in_its_own_batch_leaves_nothing_held() {
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(60, 12), None);
    let (tx, rx) = mpsc::channel();
    tx.send(quit()).unwrap_or_else(|err| panic!("send: {err}"));
    drop(tx);
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("quit-run".to_owned())
        .spawn(move || {
            let code = lp.run(&rx);
            done.send((code, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, lp) = finished
        .recv_timeout(DRAIN)
        .unwrap_or_else(|err| panic!("waited {DRAIN:?} for the loop to quit: {err}"));
    assert_eq!(code, 0);
    assert!(!lp.app.pages().holding());
}

/// A hub batch holds while it folds and settles once at its end: two
/// changed lines count the open page once, and the hold is released.
#[test]
fn a_hub_batch_holds_while_folding() {
    let (mut lp, _) = super::tests::new_loop(TestBackend::new(60, 12), None);
    lp.app.attach(contract::SessionId(SESSION.to_owned()));
    let mut seq = 0u64;
    let mut turn = |kind: &str, action: Option<&str>, payload: Value| {
        let envelope = line(kind, Some(seq), action, payload);
        seq += 1;
        Input::Hub(Line::Session(envelope))
    };
    let (tx, rx) = mpsc::channel();
    tx.send(turn(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "prompt"}]}]}),
    ))
    .unwrap_or_else(|err| panic!("send: {err}"));
    tx.send(turn(
        "text_completed",
        Some("a_m"),
        json!({"text": "marker one"}),
    ))
    .unwrap_or_else(|err| panic!("send: {err}"));
    tx.send(quit()).unwrap_or_else(|err| panic!("send: {err}"));
    drop(tx);
    let before = lp.app.pages().recounts;
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("hub-run".to_owned())
        .spawn(move || {
            let code = lp.run(&rx);
            done.send((code, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let (code, lp) = finished
        .recv_timeout(DRAIN)
        .unwrap_or_else(|err| panic!("waited {DRAIN:?} for the loop to quit: {err}"));
    assert_eq!(code, 0);
    // Both lines folded before the count: one recount for the batch.
    assert_eq!(lp.app.pages().recounts - before, 1);
    assert!(!lp.app.pages().holding());
    let screen = shown(&lp);
    assert!(screen.contains("marker one"), "{screen}");
}
