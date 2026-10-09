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
        paste_reader: None,
        pointer: crate::mouse::Pointer::default(),
        hover: true,
        var: Box::new(|_| None),
        copy_command: None,
        open_command: None,
        title: crate::osc::Title::default(),
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
        rows.first().is_some_and(|row| row.trim_start().starts_with('▄')),
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
            | Input::Files { .. }
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
    // draws fewer rows. Six turns draw one time row and two edge rows each.
    assert!(report.contains("rows: 331\n"), "{report}");
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
