//! The attach workload (`docs/performance.md`, "Budgets"): the terminal to
//! its first frame attaching, for a live, idle session whose log is 1 MiB
//! and one whose log is 10 MiB. Each fixture's log is grown to its band:
//! two probe sessions measure one turn's log and each further turn's, then
//! the fixture takes whole turns to its band. The terminal attaches once
//! untimed and `runs` times timed, from just before spawning
//! `fiber resume <id>` to the session's last reply on screen.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};

use crate::busy::{self, BYTES_PER_TOKEN, READY, TRIGGER_TOKENS, in_home};
use crate::home::Home;
use crate::idle::{self, Ctx, HubExit, Samples, Workload, ms};
use crate::pty::Terminal;
use crate::resume::{self, Fixture};
use crate::run::{self, Client, Proc, Session};

pub(crate) const SAMPLED: [Workload; 1] = [Workload {
    name: "terminal attach",
    timing: true,
    run: terminal_attach,
}];

/// The last reply's text, which no other screen shows: the attach ends when
/// the terminal holds it.
pub(crate) const ATTACH_TAIL: &str = "quokkas";

/// The in-process replay draws at the pty's size: 60 columns by 12 rows
/// (`pty.rs` sizes its terminal the same).
const OPEN_WIDTH: u16 = 60;
const OPEN_HEIGHT: u16 = 12;

/// The reply bytes of one probe or growth turn: about one screenful, a
/// typical reply. Small replies keep redrawing each reply's markdown fast
/// while the log replays, and one turn's log stays far below either band's
/// one-MiB width, so rounding up to whole turns lands the log inside its
/// band. Reply bytes never size a log: the probe's measured bytes do.
pub(crate) const TURN_REPLY_BYTES: usize = 4_096;

/// A growth turn holds a fraction of the handoff trigger's tokens, so no
/// automatic handoff runs while the log grows.
const _: () = {
    assert!(TURN_REPLY_BYTES / BYTES_PER_TOKEN < TRIGGER_TOKENS);
};

/// Plans by label: the band's low end, whose high end is one MiB more
/// ([`size_note`]).
const PLANS: [(&str, u64); 2] = [("1 MiB", 1_048_576), ("10 MiB", 10_485_760)];

/// The probes: one turn, then two turns with a handoff, each of
/// [`TURN_REPLY_BYTES`]. Their two log sizes split the fixed cost from the
/// cost of each further turn with its handoff.
pub(crate) const PROBE_A: Fixture = Fixture {
    metric: "attach_probe",
    turns: 1,
    reply_bytes: TURN_REPLY_BYTES,
    handoffs: 0,
};
pub(crate) const PROBE_B: Fixture = Fixture {
    metric: "attach_probe",
    turns: 2,
    reply_bytes: TURN_REPLY_BYTES,
    handoffs: 1,
};

/// The turns growing from a `first`-byte start in `extra`-byte steps need
/// to reach `lo`: one plus the shortfall in whole turns.
pub(crate) fn plan_turns(lo: u64, first: u64, extra: u64) -> Result<u64, String> {
    if extra == 0 {
        return Err("a growth turn logged no bytes".to_owned());
    }
    Ok(if lo <= first {
        1
    } else {
        1 + (lo - first).div_ceil(extra)
    })
}

/// A note when the `fixture` log at `log_bytes` is outside its band, and
/// none inside it: 1 MiB in [1,048,576, 2,097,152), 10 MiB in
/// [10,485,760, 11,534,336).
pub(crate) fn size_note(fixture: &str, log_bytes: u64) -> Option<String> {
    let (low, high) = match fixture {
        "1 MiB" => (1_048_576, 2_097_152),
        "10 MiB" => (10_485_760, 11_534_336),
        _ => return Some(format!("unknown fixture {fixture:?}")),
    };
    if log_bytes >= low && log_bytes < high {
        None
    } else {
        Some(format!("{log_bytes} bytes is outside {fixture}'s band"))
    }
}

/// What one hub feed line says about the session coming up.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Nothing yet: another line, or another session's.
    Wait,
    /// The hub lists it idle while its process runs.
    Ready,
    /// The hub reported it left, with why.
    Gone(String),
}

/// Feeds one hub feed line for `id`: `session_left` for it is [`Step::Gone`];
/// an idle `session_status` for it is [`Step::Ready`] only while `alive` is
/// true, the owned session's process not having exited. The hub sends a left
/// session's last status before its `session_left`, so the status alone does
/// not prove the session is up.
pub(crate) fn feed_step(line: &Value, id: &str, alive: bool) -> Step {
    match line.get("kind").and_then(Value::as_str) {
        Some("session_left") if line.pointer("/payload/session_id") == Some(&json!(id)) => {
            let how = line.pointer("/payload/how").and_then(Value::as_str);
            Step::Gone(format!("{id} {}", how.unwrap_or("left")))
        }
        Some("session_status")
            if line.get("session_id") == Some(&json!(id))
                && line.pointer("/payload/state") == Some(&json!("idle"))
                && alive =>
        {
            Step::Ready
        }
        Some(_) | None => Step::Wait,
    }
}

/// Until the hub's feed lists `id` idle while its process runs: the session
/// is live, idle, with no client.
fn wait_listed(ctx: &Ctx<'_>, home: &Home, id: &str, proc: &mut Proc) -> Result<(), String> {
    let mut feed = Client::connect(&home.hub_socket())?;
    feed.send(r#"{"id":"c_feed","command":"feed"}"#)?;
    let until = ctx.clock.now() + READY;
    loop {
        let line = feed.line(ctx.clock, until, &format!("the hub to list {id} as idle"))?;
        match feed_step(&line, id, proc.running()?) {
            Step::Ready => return Ok(()),
            Step::Gone(why) => return Err(why),
            Step::Wait => {}
        }
    }
}

/// The session's `events.jsonl` path.
fn log_path(home: &Home, id: &str) -> std::path::PathBuf {
    log::sessions_dir(&home.home(), &doors::project(&home.workspace()))
        .join(id)
        .join("events.jsonl")
}

/// The session's `events.jsonl` size, read just before its attach.
fn read_log_bytes(home: &Home, id: &str) -> Result<u64, String> {
    let path = log_path(home, id);
    fs::metadata(&path)
        .map(|found| found.len())
        .map_err(|err| format!("reading {}: {err}", path.display()))
}

/// How long one open-mode jig run may take: the release-built jig
/// replays a 10 MiB log and draws its frames.
pub(crate) const OPEN_JIG: Duration = Duration::from_secs(120);

/// What one open-mode jig run printed: parsing each line, folding it,
/// and the frames drawn.
#[derive(Debug, PartialEq)]
pub(crate) struct OpenFigures {
    pub(crate) parse_ms: f64,
    pub(crate) fold_ms: f64,
    pub(crate) frames: usize,
    pub(crate) frame_ms: f64,
}

/// The jig at `jig` reopening `log` at `width` by `height`, drawing a
/// frame after every `frame_every` lines. The environment holds `PATH`
/// alone, as the paging workload's jig command does.
pub(crate) fn open_command(
    jig: &Path,
    log: &Path,
    width: u16,
    height: u16,
    frame_every: &str,
    path: Option<&OsStr>,
) -> Command {
    let mut command = Command::new(jig);
    command
        .arg("open")
        .arg(log)
        .arg(width.to_string())
        .arg(height.to_string())
        .arg(frame_every)
        .env_clear()
        .env("PATH", path.unwrap_or_default());
    command
}

/// The milliseconds `key` holds: finite and not negative.
fn open_ms(line: &Value, key: &str) -> Result<f64, String> {
    match line.get(key).and_then(Value::as_f64) {
        Some(value) if value.is_finite() && value >= 0.0 => Ok(value),
        _ => Err(format!(
            "the paging jig's open line has no {key:?} time: {line}"
        )),
    }
}

/// The jig's one JSON line, into what it measured.
pub(crate) fn parse_open_line(line: &str) -> Result<OpenFigures, String> {
    let parsed: Value = serde_json::from_str(line)
        .map_err(|err| format!("the paging jig's open line is not JSON ({err}): {line:?}"))?;
    let frames = parsed
        .get("frames")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("the paging jig's open line has no \"frames\" count: {parsed}"));
    Ok(OpenFigures {
        parse_ms: open_ms(&parsed, "parse_ms")?,
        fold_ms: open_ms(&parsed, "fold_ms")?,
        frames: usize::try_from(frames?)
            .map_err(|_| format!("the paging jig's open line has no \"frames\" count: {parsed}"))?,
        frame_ms: open_ms(&parsed, "frame_ms")?,
    })
}

/// One open-mode run over `log`, drawing a frame after every
/// `frame_every` lines. The jig prints one JSON line; anything else,
/// or a jig that failed, is an error carrying its stderr.
fn open_run(ctx: &Ctx<'_>, log: &Path, frame_every: &str) -> Result<OpenFigures, String> {
    let jig = ctx.paging.ok_or("the attach workload needs --paging")?;
    let mut command = open_command(
        jig,
        log,
        OPEN_WIDTH,
        OPEN_HEIGHT,
        frame_every,
        ctx.path.as_deref(),
    );
    let finished = run::run_to_end(
        &mut command,
        ctx.clock,
        OPEN_JIG,
        "the paging jig's open run",
    )?;
    if !finished.status.success() {
        return Err(format!(
            "the paging jig's open run exited {}; stderr: {}",
            finished.status,
            finished.stderr.trim()
        ));
    }
    let lines: Vec<&str> = finished
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let [line] = lines.as_slice() else {
        return Err(format!(
            "the paging jig's open run printed {} lines, not one; stderr: {}",
            lines.len(),
            finished.stderr.trim()
        ));
    };
    parse_open_line(line)
}

/// One attach, untimed: warming whatever the timed ones share. The wait's
/// error already carries the screen captured at the deadline.
fn attach_once(
    ctx: &Ctx<'_>,
    home: &Home,
    id: &str,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let terminal = Terminal::spawn(home, &["resume", id], ctx.path.as_deref(), ctx.clock)?;
    let started = terminal.proc.spawned;
    let shown = terminal.wait_for(ctx.clock, started + READY, ATTACH_TAIL);
    idle::quit(ctx, terminal, notes, HubExit::Keep)?;
    shown
}

/// One timed attach: the log's size just before the spawn, then spawn to the
/// tail on screen.
fn sample(
    ctx: &Ctx<'_>,
    home: &Home,
    label: &str,
    fixture: &Fixture,
    id: &str,
    notes: &mut Vec<String>,
) -> Result<Samples, String> {
    let log_bytes = read_log_bytes(home, id)?;
    let terminal = Terminal::spawn(home, &["resume", id], ctx.path.as_deref(), ctx.clock)?;
    let started = terminal.proc.spawned;
    let shown = terminal.wait_for(ctx.clock, started + READY, ATTACH_TAIL);
    let took = ms(ctx.clock, started);
    idle::quit(ctx, terminal, notes, HubExit::Keep)?;
    shown?;
    if let Some(note) = size_note(label, log_bytes) {
        notes.push(note);
    }
    let terminal_ms = took.as_f64().ok_or("the attach timing is not a number")?;
    let mut samples = vec![(
        fixture.metric,
        json!({"fixture": label, "log_bytes": log_bytes, "ms": took}),
    )];
    let hub_ms = hub_replay(ctx, home, id)?;
    if ctx.paging.is_none() {
        // Without the release-built jig the log's stages stay unmeasured;
        // the terminal and hub rows still stand on their own.
        notes.push(
            "the paging jig was not given; parse, fold and frames stages are missing".to_owned(),
        );
        samples.extend(attach_stage_rows(
            label,
            log_bytes,
            terminal_ms,
            hub_ms,
            None,
        ));
        return Ok(samples);
    }
    let log = log_path(home, id);
    // usize::MAX draws the single final frame, as the jig's `end` does.
    // An open run that errs keeps the valid terminal and hub rows: the
    // base jig predates `open` mode, so the base run fails here.
    let opens = (|| {
        let one = open_run(ctx, &log, &usize::MAX.to_string())?;
        // 4,096 is the loop's HUB_BATCH (`crates/tui/src/event_loop/batch.rs`),
        // the lines each frame folds while the log streams in.
        let batched = open_run(ctx, &log, "4096")?;
        let dense = open_run(ctx, &log, "64")?;
        Ok::<_, String>((one, batched, dense))
    })();
    match opens {
        Ok((one, batched, dense)) => {
            samples.extend(attach_stage_rows(
                label,
                log_bytes,
                terminal_ms,
                hub_ms,
                Some((&one, &batched, &dense)),
            ));
        }
        Err(error) => {
            let (rows, note) = open_error_samples(label, log_bytes, terminal_ms, hub_ms, &error);
            notes.push(note);
            samples.extend(rows);
        }
    }
    Ok(samples)
}

/// The attach's stage rows: the terminal and hub rows always, and the
/// jig's parse, fold and frame rows when its three runs measured them.
/// Parsing and folding cost the same at any frame count, so the
/// single-frame run's hold for every frame stage.
pub(crate) fn attach_stage_rows(
    label: &str,
    log_bytes: u64,
    terminal_ms: f64,
    hub_ms: f64,
    opens: Option<(&OpenFigures, &OpenFigures, &OpenFigures)>,
) -> Samples {
    let Some((one, batched, dense)) = opens else {
        return vec![
            (
                "attach_stage_ms",
                json!({"fixture": label, "log_bytes": log_bytes, "stage": "terminal", "ms": terminal_ms}),
            ),
            (
                "attach_stage_ms",
                json!({"fixture": label, "log_bytes": log_bytes, "stage": "hub_replay", "ms": hub_ms}),
            ),
        ];
    };
    stage_rows(
        label,
        log_bytes,
        terminal_ms,
        hub_ms,
        one.parse_ms,
        one.fold_ms,
        &[
            ("frames_1", one.frames, one.frame_ms),
            ("frames_4096", batched.frames, batched.frame_ms),
            ("frames_64", dense.frames, dense.frame_ms),
        ],
    )
}

/// What the attach records when an open-mode jig run errs: the terminal
/// and hub rows only (the same rows the no-jig path emits), with a note
/// naming the unavailable stages and the jig's error. The bench harness
/// also runs the base binary with the base jig, which predates `open`
/// mode and fails on it; the failure must not lose the valid terminal
/// and hub rows.
pub(crate) fn open_error_samples(
    label: &str,
    log_bytes: u64,
    terminal_ms: f64,
    hub_ms: f64,
    error: &str,
) -> (Samples, String) {
    let rows = attach_stage_rows(label, log_bytes, terminal_ms, hub_ms, None);
    let note = format!(
        "the paging jig's open run failed ({error}); parse, fold, frames_1, frames_4096 and frames_64 stages are missing"
    );
    (rows, note)
}

/// One `attach_stage_ms` sample per stage: the terminal's spawn-to-tail
/// time, the hub replay, the replay's parse and fold, and each frame
/// count's total frame time with its frame count.
pub(crate) fn stage_rows(
    label: &str,
    log_bytes: u64,
    terminal_ms: f64,
    hub_ms: f64,
    parse_ms: f64,
    fold_ms: f64,
    frames: &[(&'static str, usize, f64)],
) -> Samples {
    let mut rows = vec![
        (
            "attach_stage_ms",
            json!({"fixture": label, "log_bytes": log_bytes, "stage": "terminal", "ms": terminal_ms}),
        ),
        (
            "attach_stage_ms",
            json!({"fixture": label, "log_bytes": log_bytes, "stage": "hub_replay", "ms": hub_ms}),
        ),
        (
            "attach_stage_ms",
            json!({"fixture": label, "log_bytes": log_bytes, "stage": "parse", "ms": parse_ms}),
        ),
        (
            "attach_stage_ms",
            json!({"fixture": label, "log_bytes": log_bytes, "stage": "fold", "ms": fold_ms}),
        ),
    ];
    for (stage, count, ms) in frames {
        rows.push((
            "attach_stage_ms",
            json!({"fixture": label, "log_bytes": log_bytes, "stage": stage, "ms": ms, "frames": count}),
        ));
    }
    rows
}

/// Whether the hub replay has finished: the subscribe is acknowledged
/// and this line carries the fixture's last reply. The hub writes the
/// acknowledgement before it starts the event writer, so the
/// acknowledgement alone never finishes the replay.
pub(crate) fn replay_finished(acked: bool, line: &Value) -> bool {
    acked && line_has_tail(line)
}

/// Whether any text in `line` holds the fixture's last reply.
fn line_has_tail(line: &Value) -> bool {
    match line {
        Value::String(text) => text.contains(ATTACH_TAIL),
        Value::Array(items) => items.iter().any(line_has_tail),
        Value::Object(map) => map.values().any(line_has_tail),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// One `subscribe` at `full` over a raw hub-socket client, from the send
/// to the fixture's last reply: the hub and session read, serialise and
/// socket half of an attach, with no terminal. The line carries the
/// session id, as the terminal's own subscribe does. The client is
/// dropped, so the session is idle with no client for the next sample.
fn hub_replay(ctx: &Ctx<'_>, home: &Home, id: &str) -> Result<f64, String> {
    let command = doors::mint("c_");
    let line =
        json!({"id": command, "command": "subscribe", "session_id": id, "args": {"level": "full"}})
            .to_string();
    let mut client = Client::connect(&home.hub_socket())?;
    let started = ctx.clock.now();
    client.send(&line)?;
    let until = started + READY;
    let what = "the hub replay of the fixture's last reply";
    let mut acked = false;
    loop {
        let got = client.line(ctx.clock, until, what)?;
        if !acked && got.pointer("/payload/command_id") == Some(&json!(command)) {
            if got.get("kind") != Some(&json!("command_accepted")) {
                return Err(format!("the hub replay subscribe was not accepted: {got}"));
            }
            acked = true;
        }
        if replay_finished(acked, &got) {
            break;
        }
    }
    let took = ctx
        .clock
        .now()
        .saturating_duration_since(started)
        .as_secs_f64()
        * 1000.0;
    drop(client);
    Ok(took)
}

/// Samples one fixture: its log generated by the binary under test, then the
/// hub and one live, idle session, held while the terminal attaches.
fn attach_fixture(
    ctx: &Ctx<'_>,
    home: &Home,
    label: &str,
    fixture: &Fixture,
    notes: &mut Vec<String>,
) -> Result<Samples, String> {
    let id = doors::mint("s_");
    resume::generate(ctx, home, fixture, &id, notes)?;
    let hub = busy::start_hub(ctx, home)?;
    let measured: Result<Samples, String> = (|| {
        let mut session =
            Session::spawn(&mut busy::session(ctx, home, &id, &["--resume"]), ctx.clock)?;
        session.line(ctx.clock, ctx.clock.now() + READY, "the first stdout line")?;
        let mut client = resume::subscribe(ctx, home, &id)?;
        let prompt = json!({"content": [{"type": "text", "text": "are you there"}]});
        resume::run_turn(
            ctx,
            &mut client,
            "c_attach",
            &resume::command("c_attach", "prompt", &prompt),
        )?;
        drop(client);
        wait_listed(ctx, home, &id, &mut session.proc)?;
        attach_once(ctx, home, &id, notes)?;
        let mut samples = Vec::new();
        for _ in 0..ctx.runs {
            samples.extend(sample(ctx, home, label, fixture, &id, notes)?);
        }
        if !session.proc.running()? {
            notes.push(format!("{id} exited while it was sampled"));
        } else {
            wait_listed(ctx, home, &id, &mut session.proc)?;
        }
        session.proc.stop(ctx.clock)?;
        Ok(samples)
    })();
    let stopped = hub.stop(ctx.clock);
    let samples = measured?;
    stopped?;
    Ok(samples)
}

/// Measures one turn's log and each further turn's, in a home of its own:
/// two probe sessions by the binary under test, `(first, extra)`.
fn probe_turn_bytes(ctx: &Ctx<'_>, notes: &mut Vec<String>) -> Result<(u64, u64), String> {
    let mut script = PROBE_A.script_prefix();
    script.extend(PROBE_B.script_prefix());
    let home = Home::scripted(ctx.home.fiber(), script)?;
    let (mut first, mut extra) = (0, 0);
    in_home(home, |home| {
        let a = doors::mint("s_");
        resume::generate(ctx, home, &PROBE_A, &a, notes)?;
        first = read_log_bytes(home, &a)?;
        let b = doors::mint("s_");
        resume::generate(ctx, home, &PROBE_B, &b, notes)?;
        extra = read_log_bytes(home, &b)?.saturating_sub(first);
        Ok(vec![])
    })?;
    Ok((first, extra))
}

/// Samples both fixtures, each grown to its band in a home of its own.
fn terminal_attach(ctx: &Ctx<'_>, notes: &mut Vec<String>) -> Result<Samples, String> {
    let (first, extra) = probe_turn_bytes(ctx, notes)?;
    let mut samples = Vec::new();
    for (label, lo) in PLANS {
        let turns = usize::try_from(plan_turns(lo, first, extra)?)
            .map_err(|err| format!("counting turns: {err}"))?;
        let fixture = Fixture {
            metric: "terminal_attach_ms",
            turns,
            reply_bytes: TURN_REPLY_BYTES,
            handoffs: turns - 1,
        };
        let home = Home::scripted(ctx.home.fiber(), fixture.script_with_last(ATTACH_TAIL))?;
        samples.extend(in_home(home, |home| {
            attach_fixture(ctx, home, label, &fixture, notes)
        })?);
    }
    Ok(samples)
}

#[cfg(test)]
#[path = "attach_tests.rs"]
mod tests;
