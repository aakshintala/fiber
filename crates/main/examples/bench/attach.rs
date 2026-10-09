//! The attach workload (`docs/performance.md`, "Budgets"): the terminal to
//! its first frame attaching, for a live, idle session whose log is 1 MiB
//! and one whose log is 10 MiB. Each fixture's log is grown to its band:
//! two probe sessions measure one turn's log and each further turn's, then
//! the fixture takes whole turns to its band. The terminal attaches once
//! untimed and `runs` times timed, from just before spawning
//! `fiber resume <id>` to the session's last reply on screen.

use std::fs;

use serde_json::{Value, json};

use crate::busy::{self, BYTES_PER_TOKEN, READY, TRIGGER_TOKENS, in_home};
use crate::home::Home;
use crate::idle::{self, Ctx, HubExit, Samples, Workload, ms};
use crate::pty::Terminal;
use crate::resume::{self, Fixture};
use crate::run::{Client, Proc, Session};

pub(crate) const SAMPLED: [Workload; 1] = [Workload {
    name: "terminal attach",
    timing: true,
    run: terminal_attach,
}];

/// The last reply's text, which no other screen shows: the attach ends when
/// the terminal holds it.
pub(crate) const ATTACH_TAIL: &str = "quokkas";

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

/// The session's `events.jsonl` size, read just before its attach.
fn read_log_bytes(home: &Home, id: &str) -> Result<u64, String> {
    let path = log::sessions_dir(&home.home(), &doors::project(&home.workspace()))
        .join(id)
        .join("events.jsonl");
    fs::metadata(&path)
        .map(|found| found.len())
        .map_err(|err| format!("reading {}: {err}", path.display()))
}

/// One attach, untimed: warming whatever the timed ones share.
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
    Ok(vec![(
        fixture.metric,
        json!({"fixture": label, "log_bytes": log_bytes, "ms": took}),
    )])
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
