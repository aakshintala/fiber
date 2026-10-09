//! The attach workload (`docs/performance.md`, "Budgets"): the terminal to
//! its first frame attaching, for a live, idle session whose log is 1 MiB
//! and one whose log is 10 MiB. Each fixture is generated once, then the
//! terminal attaches once untimed and `runs` times timed, from just before
//! spawning `fiber resume <id>` to the session's last reply on screen.

use std::fs;

use serde_json::{Value, json};

use crate::busy::{self, READY, in_home};
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

/// One turn of 1,048,576 reply bytes: a 1 MiB log, with no handoff.
pub(crate) const ONE_MIB: Fixture = Fixture {
    metric: "terminal_attach_ms",
    turns: 1,
    reply_bytes: 1_048_576,
    handoffs: 0,
};

/// Ten such turns, a handoff after each of the first nine: a 10 MiB log,
/// each turn under the handoff trigger.
pub(crate) const TEN_MIB: Fixture = Fixture {
    metric: "terminal_attach_ms",
    turns: 10,
    reply_bytes: 1_048_576,
    handoffs: 9,
};

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

/// Samples both fixtures, each in a home of its own.
fn terminal_attach(ctx: &Ctx<'_>, notes: &mut Vec<String>) -> Result<Samples, String> {
    let mut samples = Vec::new();
    for (label, fixture) in [("1 MiB", &ONE_MIB), ("10 MiB", &TEN_MIB)] {
        let home = Home::scripted(ctx.home.fiber(), fixture.script_with_last(ATTACH_TAIL))?;
        samples.extend(in_home(home, |home| {
            attach_fixture(ctx, home, label, fixture, notes)
        })?);
    }
    Ok(samples)
}

#[cfg(test)]
#[path = "attach_tests.rs"]
mod tests;
