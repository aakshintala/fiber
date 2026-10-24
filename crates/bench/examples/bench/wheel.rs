//! The wheel workload (`docs/performance.md`, "Budgets"): a scripted burst
//! of wheel-up reports at a fixed rate against a `fiber` binary under a
//! pseudo-terminal at 160x48, in two sessions: the `paging` jig's session
//! at scale 1, and one turn with a 150-item reply and an opened `read` of
//! 300 lines, of which Fiber draws the first 20. Each run records, per
//! report, the time from writing the report to the frame showing the
//! scrolled rows, plus every frame's bytes.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::events::{
    AssistantMessageCompleted, CallStatus, Class, Empty, Event, InputItem, MessageOutcome,
    SessionStarted, TextCompleted, ToolCallCompleted, ToolCallRequested, ToolCallStarted,
    TurnCompleted, TurnOutcome, TurnStarted, UsageRecorded, Variables, VariablesSource,
};
use contract::shapes::{ContentPart, DeclaredEffects, Origin, Sender, Tokens};
use contract::{ActionId, CommandId, Envelope, GenerationId, SessionId, TurnId};
use serde_json::{Value, json};

use crate::attach::wait_listed;
use crate::busy::{self, READY};
use crate::home::Home;
use crate::idle::{self, Ctx, Samples, Workload};
use crate::pty::{Frame, Terminal};
use crate::run::{self, Proc, Session};

pub(crate) const SAMPLED: [Workload; 1] = [Workload {
    name: "terminal wheel",
    timing: true,
    run: terminal_wheel,
}];

/// Reports in one burst: the ticket's figure.
const REPORTS: usize = 30;
// debt: 30 reports is picked, not measured; a burst length the ticket's
// 16 ms ceiling justifies would set it.

/// One report every 8 ms: about a 120 Hz trackpad.
// debt: 8 ms is picked, not measured; a measured trackpad rate would set it.
const EVERY: Duration = Duration::from_millis(8);

/// The terminal's size, in cells.
const COLS: u16 = 160;
const ROWS: u16 = 48;

/// The pointer's 0-based cell: inside the conversation.
// debt: the cell is picked, not measured; a measured pointer rest position
// would set it.
const POINTER: (u16, u16) = (60, 10);

/// Lines in the read session's file.
// debt: 300 lines is picked, not measured; the owner's "a few hundred
// lines" would set it once measured.
const READ_LINES: usize = 300;

/// The opened output's mark: the start of every output line, drawn only
/// once the output opens. Both binaries draw it: the head draws the
/// first 20 lines, while the base follows the bottom and shows the last
/// lines. The filler reply, the prompt and the ledger row hold no "L0".
const OUTPUT_MARK: &str = "L0";

/// The rail's columns, left of the conversation: 15% of 160 (`docs/tui.md`,
/// "Layout"). Its cards show a running time, so compares skip them.
const RAIL: usize = 24;

/// Conversation columns kept for compares: every column after the rail and
/// left of the panel. The panel is 21% of 160, so 34 columns plus its edge.
const CONV: usize = COLS as usize - 35 - RAIL;

/// Silence proving the terminal settled: no new frame for this long.
const QUIESCE: Duration = Duration::from_millis(500);
// debt: 500 ms is picked, not measured; a measured settle time on the
// benchmark runner would set it.

/// One poll while waiting for frames.
const POLL: Duration = Duration::from_millis(10);

/// A wheel report at 0-based `col`, `row`: button 64 rolls up, 65 down
/// (`docs/tui.md`, "Mouse and hover" uses mode 1006).
fn sgr_wheel(up: bool, col: u16, row: u16) -> String {
    let button = if up { 64 } else { 65 };
    format!("\x1b[<{button};{};{}M", col + 1, row + 1)
}

/// Each row's conversation columns ([`RAIL`] skipped, then [`CONV`] kept),
/// joined with newlines: the conversation without the rail and the panel,
/// whose cards may show times that change.
fn conversation(text: &str) -> String {
    text.lines()
        .map(|line| line.chars().skip(RAIL).take(CONV).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The calibrated step showing exactly `text`, or none.
fn position(text: &str, calibrated: &[String]) -> Option<usize> {
    calibrated.iter().position(|known| known == text)
}

/// Every calibrated text differs: no step left the screen unchanged.
fn distinct(calibrated: &[String]) -> Result<(), String> {
    for (at, text) in calibrated.iter().enumerate() {
        if calibrated.iter().take(at).any(|known| known == text) {
            return Err(format!("calibration step {at} shows the same screen twice"));
        }
    }
    Ok(())
}

/// Per-report latency in ms: report k (1-based) is covered by the first
/// frame at or after its write showing step k or later. Frames must arrive
/// in non-decreasing steps within the burst.
fn latencies(
    writes_ms: &[f64],
    frames: &[(f64, usize)],
    reports: usize,
) -> Result<Vec<f64>, String> {
    let mut step = 0usize;
    for (at, index) in frames {
        if *index < step {
            return Err(format!("a frame at {at} ms steps back to step {index}"));
        }
        if *index > reports {
            return Err(format!(
                "a frame at {at} ms shows step {index} past {reports}"
            ));
        }
        step = *index;
    }
    let mut covered = Vec::with_capacity(reports);
    for k in 1..=reports {
        let wrote = writes_ms
            .get(k - 1)
            .copied()
            .ok_or_else(|| format!("report {k} of {reports} has no write time"))?;
        let at = frames
            .iter()
            .find(|(at, index)| *at >= wrote && *index >= k)
            .map(|(at, _)| *at)
            .ok_or_else(|| format!("no frame shows report {k} of {reports}"))?;
        covered.push(at - wrote);
    }
    Ok(covered)
}

/// One generator line typed for the log: the event with its turn and
/// action, or nothing when the line is skipped.
type Typed = Option<(Event, Option<TurnId>, Option<ActionId>)>;

/// One generator line as the log holds it: `None` skips the line. The
/// generator's `fiber_started` and `session_started` never reach the log
/// (the bench writes its own), ephemeral lines carry no state, and the two
/// kinds whose payloads the jig shapes for replay, not for the log, are
/// completed here. Anything else failing to parse is a setup error naming
/// the kind and line.
fn typed(line: &Envelope, n: usize) -> Result<Typed, String> {
    let failure = |why: &str| format!("session line {n} ({}): {why}", line.kind);
    match line.kind.as_str() {
        "fiber_started" | "session_started" => Ok(None),
        "tool_call_started" => Ok(Some((
            Event::ToolCallStarted(ToolCallStarted {
                always_reviewed: false,
                declared: DeclaredEffects {
                    effects: Vec::new(),
                    reversible: true,
                    paths: None,
                },
                arguments: None,
                changed_by: None,
            }),
            line.turn_id.clone(),
            line.action_id.clone(),
        ))),
        "usage_recorded" => Ok(Some((
            Event::UsageRecorded(UsageRecorded {
                generation_id: GenerationId(format!("g_{n}")),
                model: "fake/m".to_owned(),
                tokens: Tokens {
                    input: 1200,
                    output: 300,
                    cache_read: 0,
                    cache_write: BTreeMap::new(),
                },
                input_bytes: 4800,
                input_media: None,
                web_searches: None,
                cost: None,
                subscription: None,
                extension: None,
                origin_session_id: None,
                reviewer: None,
            }),
            line.turn_id.clone(),
            line.action_id.clone(),
        ))),
        _ => match Event::from_envelope(line) {
            Err(err) => Err(failure(&err.to_string())),
            Ok(None) => Err(failure("the build does not know this kind")),
            Ok(Some(event)) => {
                if event.class() == Class::Ephemeral {
                    Ok(None)
                } else {
                    Ok(Some((event, line.turn_id.clone(), line.action_id.clone())))
                }
            }
        },
    }
}

/// The workload's two sample rows: the slowest report's ms under the
/// session's metric, and the counts object with the per-report latencies,
/// every frame's ms, bytes and calibrated index, and the calibration
/// single-step latencies.
fn session_samples(
    session: &str,
    slowest_ms: f64,
    covered: &[f64],
    frames: &[FrameSample],
    steps_ms: &[f64],
) -> Samples {
    let metric = if session == "paging" {
        "wheel_paging_ms"
    } else {
        "wheel_read_ms"
    };
    let frames: Vec<Value> = frames
        .iter()
        .map(|(ms, bytes, index)| json!({"ms": ms, "bytes": bytes, "index": index}))
        .collect();
    vec![
        (metric, json!(slowest_ms)),
        (
            "wheel_counts",
            json!({
                "session": session,
                "reports": REPORTS,
                "latencies_ms": covered,
                "frames": frames,
                "step_ms": steps_ms,
            }),
        ),
    ]
}

/// The sessions directory of `home`'s workspace.
fn sessions_dir(home: &Home) -> std::path::PathBuf {
    log::sessions_dir(&home.home(), &doors::project(&home.workspace()))
}

/// A new log for `id`, starting from the bench's own `SessionStarted`.
fn create_log(home: &Home, id: &str) -> Result<log::Log, String> {
    let sessions = sessions_dir(home);
    let clock: Arc<dyn Clock> = Arc::new(run::System);
    let log = log::Log::create(&sessions, SessionId(id.to_owned()), clock)
        .map_err(|err| format!("creating session {id}: {err:?}"))?;
    let workspace = fs::canonicalize(home.workspace())
        .map_err(|err| format!("reading the workspace: {err}"))?
        .display()
        .to_string();
    log.append(
        &Event::SessionStarted(SessionStarted {
            supervised: false,
            workspace,
            variables: Variables {
                path: String::new(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: None,
            rewind: None,
            worktree: None,
        }),
        None,
        None,
    )
    .map_err(|err| format!("starting session {id}: {err:?}"))?;
    Ok(log)
}

/// The paging session at scale 1, written straight to a new log: the
/// durable lines of the 10 completed turns, no deltas and not the running
/// turn. The TUI draws what the jig draws from the same generator.
fn write_paging_log(home: &Home, id: &str) -> Result<(), String> {
    let log = create_log(home, id)?;
    let mut turns = 0usize;
    for (n, line) in tui::paging_session(1).lines().enumerate() {
        let envelope: Envelope = serde_json::from_str(line)
            .map_err(|err| format!("session line {n} is not an envelope ({err})"))?;
        if envelope.kind == "turn_started" {
            turns += 1;
        }
        if turns > 10 {
            continue;
        }
        let Some((event, turn, action)) = typed(&envelope, n)? else {
            continue;
        };
        log.append(&event, turn, action)
            .map_err(|err| format!("session line {n} does not append ({err:?})"))?;
    }
    if turns != 11 {
        return Err(format!("the paging session holds {turns} turns, not 11"));
    }
    Ok(())
}

/// One turn with a `read` of [`READ_LINES`] lines, written straight to a
/// new log: the 150-item reply, the call, its output, and the turn's
/// close. The reply keeps 90 rows to scroll above the bottom once the
/// opened call draws only its first 20 lines.
fn write_read_log(home: &Home, id: &str) -> Result<(), String> {
    let log = create_log(home, id)?;
    let reply = (1..=150)
        .map(|n| format!("- T{n:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    let output = (1..=READ_LINES)
        .map(|n| format!("L{n:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    let message = ActionId("a_m1".to_owned());
    let call = ActionId("a_t1".to_owned());
    let events: Vec<(Event, Option<TurnId>, Option<ActionId>)> = vec![
        (
            Event::TurnStarted(TurnStarted {
                input: vec![InputItem::Message {
                    content: vec![ContentPart::Text {
                        text: "read the notes".to_owned(),
                    }],
                    sender: Sender {
                        origin: Origin::Driver,
                        command_id: Some(CommandId("c_1".to_owned())),
                    },
                    changed_by: None,
                }],
            }),
            None,
            None,
        ),
        (Event::StepStarted(Empty {}), None, None),
        (
            Event::AssistantMessageStarted(Empty {}),
            None,
            Some(message.clone()),
        ),
        (
            Event::TextCompleted(TextCompleted {
                text: reply,
                provider_item: None,
            }),
            None,
            Some(message.clone()),
        ),
        (
            Event::ToolCallRequested(ToolCallRequested {
                name: "read".to_owned(),
                arguments: json!({"path": "notes.txt"}),
                provider_id: None,
                repair: None,
                ran_by: None,
                provider_item: None,
            }),
            None,
            Some(call.clone()),
        ),
        (
            Event::AssistantMessageCompleted(AssistantMessageCompleted {
                outcome: MessageOutcome::Completed,
                error: None,
            }),
            None,
            Some(message.clone()),
        ),
        (
            Event::UsageRecorded(UsageRecorded {
                generation_id: GenerationId("g_read".to_owned()),
                model: "fake/m".to_owned(),
                tokens: Tokens {
                    input: 1200,
                    output: 300,
                    cache_read: 0,
                    cache_write: BTreeMap::new(),
                },
                input_bytes: 4800,
                input_media: None,
                web_searches: None,
                cost: None,
                subscription: None,
                extension: None,
                origin_session_id: None,
                reviewer: None,
            }),
            None,
            Some(message.clone()),
        ),
        (
            Event::ToolCallStarted(ToolCallStarted {
                always_reviewed: false,
                declared: DeclaredEffects {
                    effects: Vec::new(),
                    reversible: true,
                    paths: None,
                },
                arguments: None,
                changed_by: None,
            }),
            None,
            Some(call.clone()),
        ),
        (
            Event::ToolCallCompleted(ToolCallCompleted {
                status: CallStatus::Completed,
                reason: None,
                error: None,
                process: None,
                content: vec![ContentPart::Text { text: output }],
                details: None,
                artifact: None,
                kept: None,
                changes: None,
                control: None,
                changed_by: None,
                provider_item: None,
                hosts_surfaced: Vec::new(),
                loaded: None,
            }),
            None,
            Some(call.clone()),
        ),
        (
            Event::TurnCompleted(TurnCompleted {
                outcome: TurnOutcome::Completed,
                error: None,
                questions: None,
            }),
            None,
            None,
        ),
    ];
    for (event, turn, action) in &events {
        log.append(event, turn.clone(), action.clone())
            .map_err(|err| format!("the read session does not append ({err:?})"))?;
    }
    Ok(())
}

/// When the recorded frames arrived, oldest to newest, for a timeout
/// note: the count and the first and last stamps.
fn frame_note(frames: &[Frame]) -> String {
    match (frames.first(), frames.last()) {
        (Some(first), Some(last)) => {
            format!(
                "{} frames, first {:?}, last {:?}",
                frames.len(),
                first.at,
                last.at
            )
        }
        _ => "no frames".to_owned(),
    }
}

/// Until `until`, the recorded frames satisfying `done`: one deadline for
/// the whole wait, as [`Terminal::wait_for`] keeps.
fn wait_frames(
    clock: &dyn Clock,
    terminal: &Terminal,
    until: Instant,
    what: &str,
    done: impl Fn(&[Frame]) -> bool,
) -> Result<Vec<Frame>, String> {
    loop {
        let frames = terminal.frames();
        if done(&frames) {
            return Ok(frames);
        }
        if clock.now() >= until {
            return Err(format!(
                "timed out waiting for {what}: {}",
                frame_note(&frames)
            ));
        }
        clock.sleep(POLL);
    }
}

/// Until `until`, the live screen holding `needle`: its row and column,
/// for clicks on rows the waits locate. One deadline for the whole wait.
fn wait_text(
    clock: &dyn Clock,
    terminal: &Terminal,
    until: Instant,
    what: &str,
    needle: &str,
) -> Result<(usize, usize), String> {
    loop {
        if let Some(at) = terminal.find(needle) {
            return Ok(at);
        }
        if clock.now() >= until {
            return Err(format!("timed out waiting for {what}"));
        }
        clock.sleep(POLL);
    }
}

/// The ms from `since` to `at`.
fn ms_since(clock: &dyn Clock, since: Instant) -> f64 {
    clock.now().saturating_duration_since(since).as_secs_f64() * 1000.0
}

/// Reads the report's send time, then runs `write`, and returns that
/// time. The read comes first so a frame the reader timestamps after the
/// write still covers the report.
fn send_stamped(
    clock: &dyn Clock,
    origin: Instant,
    write: impl FnOnce() -> Result<(), String>,
) -> Result<f64, String> {
    let sent = ms_since(clock, origin);
    write()?;
    Ok(sent)
}

/// The ms from `since` to a frame's stamp.
fn frame_ms(at: Instant, since: Instant) -> f64 {
    at.saturating_duration_since(since).as_secs_f64() * 1000.0
}

/// Thirty wheel-down reports in one write, then the settled bottom: the
/// latest conversation text once no new frame arrives for [`QUIESCE`].
/// Steps that clamp at the bottom change no rows, so they draw no frame:
/// settling needs quiet, not a new frame..
fn to_bottom(clock: &dyn Clock, terminal: &mut Terminal) -> Result<String, String> {
    let base = terminal.frames().len();
    let downs = sgr_wheel(false, POINTER.0, POINTER.1).repeat(REPORTS);
    terminal.write(downs.as_bytes())?;
    let until = clock.now() + READY;
    let mut len = base;
    let mut changed = clock.now();
    loop {
        let now = terminal.frames().len();
        if now != len {
            len = now;
            changed = clock.now();
        }
        if clock.now().saturating_duration_since(changed) >= QUIESCE {
            break;
        }
        if clock.now() >= until {
            return Err(format!(
                "timed out settling at the bottom: {}",
                frame_note(&terminal.frames())
            ));
        }
        clock.sleep(POLL);
    }
    let frames = terminal.frames();
    let Some(last) = frames.last() else {
        return Err("no frame settled at the bottom".to_owned());
    };
    Ok(conversation(&last.text))
}

/// The 31 calibrated conversation texts, bottom to top, and the 30
/// single-step latencies: one wheel-up per write, each step kept only once
/// its frame's text differs, which proves no step clamped.
struct Calibration {
    bottom: String,
    texts: Vec<String>,
    steps_ms: Vec<f64>,
}

fn calibrate(
    clock: &dyn Clock,
    terminal: &mut Terminal,
    bottom: &str,
) -> Result<Calibration, String> {
    let up = sgr_wheel(true, POINTER.0, POINTER.1);
    let mut prev = bottom.to_owned();
    let mut texts = vec![prev.clone()];
    let mut steps_ms = Vec::with_capacity(REPORTS);
    for k in 1..=REPORTS {
        let base = terminal.frames().len();
        let wrote = clock.now();
        terminal.write(up.as_bytes())?;
        let until = wrote + READY;
        wait_frames(
            clock,
            terminal,
            until,
            &format!("calibration step {k}"),
            |frames| {
                frames
                    .iter()
                    .skip(base)
                    .any(|frame| conversation(&frame.text) != prev)
            },
        )?;
        let Some(frame) = terminal
            .frames()
            .iter()
            .skip(base)
            .find(|frame| conversation(&frame.text) != prev)
            .cloned()
        else {
            return Err(format!("calibration step {k} left no frame"));
        };
        steps_ms.push(frame_ms(frame.at, wrote));
        prev = conversation(&frame.text);
        texts.push(prev.clone());
    }
    distinct(&texts)?;
    Ok(Calibration {
        bottom: bottom.to_owned(),
        texts,
        steps_ms,
    })
}

/// One burst frame: its ms since the burst start, its bytes, and its
/// calibrated step.
type FrameSample = (f64, usize, usize);

/// A burst's outcome: the slowest report's ms, the per-report latencies,
/// and every frame's sample.
type Burst = (f64, Vec<f64>, Vec<FrameSample>);
/// The timed burst: 30 wheel-up reports, one per write on an absolute
/// schedule, then every frame since the burst mapped to its calibrated
/// step.
fn burst(
    clock: &dyn Clock,
    terminal: &mut Terminal,
    calibration: &Calibration,
) -> Result<Burst, String> {
    let up = sgr_wheel(true, POINTER.0, POINTER.1);
    let origin = clock.now();
    let base = terminal.frames().len();
    let mut writes_ms = Vec::with_capacity(REPORTS);
    let mut target = origin + EVERY;
    for _ in 1..=REPORTS {
        clock.sleep(target.saturating_duration_since(clock.now()));
        let sent = send_stamped(clock, origin, || terminal.write(up.as_bytes()))?;
        writes_ms.push(sent);
        target += EVERY;
    }
    wait_frames(
        clock,
        terminal,
        clock.now() + READY,
        "the burst's last step",
        |frames| {
            frames.iter().skip(base).any(|frame| {
                position(&conversation(&frame.text), &calibration.texts) == Some(REPORTS)
            })
        },
    )?;
    let mut mapped = Vec::new();
    let mut full = Vec::new();
    for frame in terminal.frames().iter().skip(base) {
        let cut = conversation(&frame.text);
        let Some(index) = position(&cut, &calibration.texts) else {
            return Err("a burst frame matches no calibrated step".to_owned());
        };
        mapped.push((frame_ms(frame.at, origin), index));
        full.push((frame_ms(frame.at, origin), frame.bytes, index));
    }
    let covered = latencies(&writes_ms, &mapped, REPORTS)?;
    if full.last().map(|(_, _, index)| *index) != Some(REPORTS) {
        return Err("the burst never showed its last step".to_owned());
    }
    let slowest_ms = covered.iter().copied().fold(0.0, f64::max);
    Ok((slowest_ms, covered, full))
}

/// Thirty wheel-down reports in one write, then the frame showing the
/// calibrated bottom again.
fn back_to_bottom(clock: &dyn Clock, terminal: &mut Terminal, bottom: &str) -> Result<(), String> {
    let base = terminal.frames().len();
    let downs = sgr_wheel(false, POINTER.0, POINTER.1).repeat(REPORTS);
    terminal.write(downs.as_bytes())?;
    wait_frames(
        clock,
        terminal,
        clock.now() + READY,
        "the return to the bottom",
        |frames| {
            frames
                .iter()
                .skip(base)
                .any(|frame| conversation(&frame.text) == bottom)
        },
    )?;
    Ok(())
}

/// A left click at 0-based `col`, `row`: the press and the release.
fn click(col: usize, row: usize) -> String {
    format!(
        "\x1b[<0;{};{}M\x1b[<0;{};{}m",
        col + 1,
        row + 1,
        col + 1,
        row + 1
    )
}

/// Opens the read session's call: the group line both binaries draw, the
/// ledger, its row, the call, and the output on screen.
fn open_read_call(
    clock: &dyn Clock,
    terminal: &mut Terminal,
    spawned: Instant,
) -> Result<(), String> {
    terminal.wait_for(clock, spawned + READY, "Read 1 file")?;
    // A running group's line names the call too ("· read notes.txt"), and
    // a click on it would close the ledger: wait for the finished turn.
    terminal.wait_for(clock, spawned + READY, "completed")?;
    terminal.write(b"\x0f")?;
    let until = clock.now() + READY;
    let (row, col) = wait_text(clock, terminal, until, "the read's ledger row", "notes.txt")?;
    terminal.write(click(col, row).as_bytes())?;
    terminal.wait_for(clock, clock.now() + READY, OUTPUT_MARK)?;
    Ok(())
}

/// How many sessions the wheel row times: the paging session, then the
/// read one, each in its own pseudo-terminal.
pub(crate) const SESSION_COUNT: u32 = 2;

/// The wheel row's sessions, in run order: the paging session draws the
/// jig's log at scale 1, and the read session opens its 300-line output
/// before the bursts.
const SESSIONS: [(&str, bool); 2] = [("paging", false), ("read", true)];

/// One wheel session, live in its own pseudo-terminal: the held terminal
/// stays attached across the session's samples, so every burst starts
/// from the calibrated bottom.
pub(crate) struct WheelSession {
    session: Session,
    terminal: Terminal,
    id: String,
    name: &'static str,
}

/// The wheel row's prepared sessions, in [`SESSIONS`] order, under one
/// hub: both sessions are live in their own pseudo-terminals before the
/// first timed burst.
pub(crate) struct WheelPrepared {
    home: Home,
    hub: Proc,
    sessions: Vec<WheelSession>,
}

/// Starts one session, attaches its terminal, and settles it at the
/// bottom: the bursts then calibrate and time from a live screen.
fn prepare_session(
    ctx: &Ctx<'_>,
    home: &Home,
    id: &str,
    name: &'static str,
    open_call: bool,
) -> Result<WheelSession, String> {
    let mut session = Session::spawn(&mut busy::session(ctx, home, id, &["--resume"]), ctx.clock)?;
    let prepared = (|| {
        session.line(ctx.clock, ctx.clock.now() + READY, "the first stdout line")?;
        wait_listed(ctx, home, id, &mut session.proc)?;
        let mut terminal = Terminal::spawn_sized(
            home,
            &["resume", id],
            ctx.path.as_deref(),
            ctx.clock,
            COLS,
            ROWS,
        )?;
        terminal.record(true);
        let spawned = terminal.proc.spawned;
        wait_frames(
            ctx.clock,
            &terminal,
            spawned + READY,
            "the first frame",
            |frames| !frames.is_empty(),
        )?;
        // The attach replays the log after its first frame: a bottom
        // taken before the finished turn draws is a blank screen.
        terminal.wait_for(ctx.clock, spawned + READY, "completed")?;
        if open_call {
            open_read_call(ctx.clock, &mut terminal, spawned)?;
        }
        let bottom = to_bottom(ctx.clock, &mut terminal)?;
        if open_call && !bottom.lines().any(|line| line.contains(OUTPUT_MARK)) {
            return Err("the bottom settled without the read's output".to_owned());
        }
        Ok(terminal)
    })();
    match prepared {
        Ok(terminal) => Ok(WheelSession {
            session,
            terminal,
            id: id.to_owned(),
            name,
        }),
        Err(err) => {
            let _stopped = session.proc.stop(ctx.clock);
            Err(err)
        }
    }
}

/// Prepares the wheel row: both sessions' logs written straight to the
/// log, one hub, and both sessions live in their own pseudo-terminals,
/// settled at the bottom. A failed prepare removes its home, so no
/// session outlives the call.
pub(crate) fn prepare_wheel(ctx: &Ctx<'_>) -> Result<WheelPrepared, String> {
    let home = ctx.home.child([])?;
    let prepared = (|| {
        let paging = doors::mint("s_");
        let read = doors::mint("s_");
        write_paging_log(&home, &paging)?;
        write_read_log(&home, &read)?;
        let hub = busy::start_hub(ctx, &home)?;
        let ids = [paging, read];
        let sessions = (|| {
            let mut sessions = Vec::new();
            for ((name, open_call), id) in SESSIONS.iter().zip(ids.iter()) {
                sessions.push(prepare_session(ctx, &home, id, name, *open_call)?);
            }
            Ok(sessions)
        })();
        match sessions {
            Ok(sessions) => Ok((hub, sessions)),
            Err(err) => {
                let _stopped = hub.stop(ctx.clock);
                Err(err)
            }
        }
    })();
    match prepared {
        Ok((hub, sessions)) => Ok(WheelPrepared {
            home,
            hub,
            sessions,
        }),
        Err(err) => {
            let _finished = home.finish();
            Err(err)
        }
    }
}

/// One timed burst against one prepared session: calibrate from the
/// settled bottom, burst, and back to the bottom, keeping the slowest
/// report. The terminal stays open for the session's next sample.
pub(crate) fn sample_wheel(
    prepared: &mut WheelPrepared,
    index: usize,
    clock: &dyn Clock,
) -> Result<Samples, String> {
    let Some(session) = prepared.sessions.get_mut(index) else {
        return Err(format!("no wheel session at index {index}"));
    };
    let bottom = to_bottom(clock, &mut session.terminal)?;
    let calibration = calibrate(clock, &mut session.terminal, &bottom)?;
    // Back to the bottom before the timed burst, so every burst frame
    // shows a calibrated step.
    back_to_bottom(clock, &mut session.terminal, &calibration.bottom)?;
    let (slowest_ms, covered, full) = burst(clock, &mut session.terminal, &calibration)?;
    back_to_bottom(clock, &mut session.terminal, &calibration.bottom)?;
    Ok(session_samples(
        session.name,
        slowest_ms,
        &covered,
        &full,
        &calibration.steps_ms,
    ))
}

/// Quits every session's terminal, stops the sessions and the hub, and
/// removes the home: the first error fails the row, as the single-call
/// run's first error did, while the remaining sessions still go away.
/// The tail check runs only when every sample ran, as the single-call
/// run never reached it after a failed sample.
pub(crate) fn finish_wheel(
    prepared: WheelPrepared,
    clock: &dyn Clock,
    notes: &mut Vec<String>,
    tail: bool,
) -> Result<(), String> {
    let WheelPrepared {
        home,
        hub,
        sessions,
    } = prepared;
    let mut result = Ok(());
    for wheel_session in sessions {
        let WheelSession {
            mut session,
            terminal,
            id,
            ..
        } = wheel_session;
        // The quit borrows the row's home, which moves into the finish
        // below.
        let row = (|| {
            let ctx = Ctx {
                home: &home,
                clock,
                idle: Duration::ZERO,
                path: None,
                runs: 1,
                paging: None,
            };
            idle::quit(&ctx, terminal, notes, idle::HubExit::Keep)?;
            if tail && !session.proc.running()? {
                notes.push(format!("{id} exited while it was sampled"));
            }
            session.proc.stop(clock)?;
            Ok(())
        })();
        if result.is_ok() {
            result = row;
        }
    }
    let stopped = hub.stop(clock);
    let finished = home.finish();
    result.and(stopped).and(finished)
}

/// Times one prepared session `runs` times: the single-call run keeps
/// its old shape, session by session, on top of the shared prepare and
/// finish.
fn measure(prepared: &mut WheelPrepared, clock: &dyn Clock, runs: u32) -> Result<Samples, String> {
    let mut samples = Samples::new();
    for index in 0..prepared.sessions.len() {
        for _ in 0..runs {
            samples.extend(sample_wheel(prepared, index, clock)?);
        }
    }
    Ok(samples)
}

/// Samples the wheel workload: both sessions' logs written straight to the
/// log, then a live, idle session each under one hub.
fn terminal_wheel(ctx: &Ctx<'_>, notes: &mut Vec<String>) -> Result<Samples, String> {
    let mut prepared = prepare_wheel(ctx)?;
    let measured = measure(&mut prepared, ctx.clock, ctx.runs);
    // The row's sessions and hub go away even when a sample errs, as the
    // old single-call run removed them before; a failed sample skips
    // the tail check it never reached.
    let tail = measured.is_ok();
    let finished = finish_wheel(prepared, ctx.clock, notes, tail);
    match measured {
        Ok(samples) => finished.map(|()| samples),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
#[path = "wheel_tests.rs"]
mod tests;
