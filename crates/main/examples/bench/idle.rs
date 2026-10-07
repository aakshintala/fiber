//! The idle and startup workloads: an idle headless session (peak RSS,
//! context switches, threads with no client and with one), an idle
//! terminal (peak RSS, context switches), session start and the terminal's
//! first frame (`docs/performance.md`, "Budgets").

use std::ffi::OsString;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use serde_json::{Value, json};

use crate::home::Home;
use crate::linux;
use crate::pty::Terminal;
use crate::run::{self, Client, Session};

/// How long `fiber` may take to reach a readiness signal: a stdout line,
/// the first frame, a socket line.
const READY: Duration = Duration::from_secs(30);

/// How long the hub may take to remove its socket after the terminal exits:
/// it idles out a second after its last client leaves.
const HUB_EXIT: Duration = Duration::from_secs(10);

/// What every workload runs with.
pub(crate) struct Ctx<'a> {
    pub(crate) home: &'a Home,
    pub(crate) clock: &'a dyn Clock,
    pub(crate) idle: Duration,
    pub(crate) path: Option<OsString>,
}

/// One run's samples, by metric id.
pub(crate) type Samples = Vec<(&'static str, Value)>;

/// A workload: its name, whether it measures a timing budget (the only
/// workloads the base binary runs), and one run, which notes self-check
/// failures and errs when it cannot finish.
pub(crate) struct Workload {
    pub(crate) name: &'static str,
    pub(crate) timing: bool,
    pub(crate) run: fn(&Ctx<'_>, &mut Vec<String>) -> Result<Samples, String>,
}

pub(crate) const WORKLOADS: [Workload; 4] = [
    Workload {
        name: "session start",
        timing: true,
        run: session_start,
    },
    Workload {
        name: "terminal first frame",
        timing: true,
        run: terminal_first_frame,
    },
    Workload {
        name: "session idle",
        timing: false,
        run: session_idle,
    },
    Workload {
        name: "terminal idle",
        timing: false,
        run: terminal_idle,
    },
];

fn ms(clock: &dyn Clock, since: Instant) -> Value {
    json!(clock.now().saturating_duration_since(since).as_secs_f64() * 1000.0)
}

fn start_session(ctx: &Ctx<'_>) -> Result<(Session, String), String> {
    let id = doors::mint("s_");
    let workspace = ctx.home.workspace();
    let mut command = run::command(
        ctx.home.fiber(),
        ctx.home.root(),
        &ctx.home.home(),
        ctx.path.as_deref(),
    );
    command
        .arg("session")
        .arg("--id")
        .arg(&id)
        .arg("--workspace")
        .arg(&workspace);
    Ok((Session::spawn(&mut command, ctx.clock)?, id))
}

/// From just before the spawn to the first complete stdout line, which
/// must be `session_started`.
fn session_start(ctx: &Ctx<'_>, _notes: &mut Vec<String>) -> Result<Samples, String> {
    let (session, _) = start_session(ctx)?;
    let started = session.proc.spawned;
    let first = session.line(ctx.clock, started + READY, "the first stdout line");
    let took = ms(ctx.clock, started);
    let stopped = session.proc.stop(ctx.clock);
    let first = first?;
    if first.get("kind") != Some(&json!("session_started")) {
        return Err(format!(
            "the first stdout line is not session_started: {first}"
        ));
    }
    stopped?;
    Ok(vec![("session_start_ms", took)])
}

/// The window starts once the session's startup has finished: the first
/// `session_status` after `extensions_loaded` is on stdout
/// ([`run::Startup`]). Threads are counted at its end with no client, then
/// again once one `full` client's writer has sent `session_status` with
/// `clients` 1: the subscribe acknowledgement is written before the writer
/// starts, so it is not that signal.
fn session_idle(ctx: &Ctx<'_>, notes: &mut Vec<String>) -> Result<Samples, String> {
    let (session, id) = start_session(ctx)?;
    let measured = measure_session(ctx, &session, &id, notes);
    let stopped = session.proc.stop(ctx.clock);
    let samples = measured?;
    stopped?;
    Ok(samples)
}

fn measure_session(
    ctx: &Ctx<'_>,
    session: &Session,
    id: &str,
    notes: &mut Vec<String>,
) -> Result<Samples, String> {
    let pid = session.proc.pid();
    session.wait_started(ctx.clock, ctx.clock.now() + READY)?;
    let (switches, rss, idle_threads) = idle_window(ctx, pid, notes)?;
    let mut client = Client::connect(&ctx.home.socket(id))?;
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#)?;
    let until = ctx.clock.now() + READY;
    let what = "session_status with clients 1";
    loop {
        let line = client.line(ctx.clock, until, what)?;
        if line.get("kind") == Some(&json!("session_status"))
            && line.pointer("/payload/clients") == Some(&json!(1))
        {
            break;
        }
    }
    let attached_threads = linux::threads(pid)?.len();
    drop(client);
    Ok(vec![
        ("session_idle_rss_kib", json!(rss)),
        ("session_idle_switches", json!(switches)),
        (
            "session_threads",
            json!([
                {"clients": 0, "threads": idle_threads},
                {"clients": 1, "threads": attached_threads},
            ]),
        ),
    ])
}

/// Reads every thread's switch counters, waits the idle window touching
/// nothing, reads them again, then reads the peak RSS. Returns the
/// per-thread deltas, the peak RSS and the thread count at the end.
fn idle_window(
    ctx: &Ctx<'_>,
    pid: u32,
    notes: &mut Vec<String>,
) -> Result<(Vec<Value>, u64, usize), String> {
    let before = linux::threads(pid)?;
    ctx.clock.sleep(ctx.idle);
    let after = linux::threads(pid)?;
    let rss = linux::peak_rss_kib(pid)?;
    Ok((
        linux::idle_switches(&before, &after, notes),
        rss,
        after.len(),
    ))
}

/// Waits until the terminal holds a connection to the hub. The terminal
/// draws nothing new when the hub connects, so the signal is the kernel's:
/// a connected socket carrying the hub socket's path.
fn hub_connected(ctx: &Ctx<'_>) -> Result<(), String> {
    let socket = ctx.home.hub_socket();
    run::poll(
        ctx.clock,
        READY,
        "the terminal to connect to the hub",
        || Ok(linux::hub_connected(&linux::net_unix()?, &socket)),
    )
}

/// Quits the terminal with two Ctrl-C, then waits for the hub to idle out
/// and remove its socket; a hub that does not is killed and noted.
fn quit(ctx: &Ctx<'_>, mut terminal: Terminal, notes: &mut Vec<String>) -> Result<(), String> {
    let typed = terminal.write(b"\x03\x03");
    let mut proc = terminal.proc;
    if typed.is_err() || !proc.exits(ctx.clock, READY)? {
        notes.push("the terminal did not quit on two Ctrl-C".to_owned());
    }
    proc.stop(ctx.clock)?;
    let socket = ctx.home.hub_socket();
    let gone = run::poll(ctx.clock, HUB_EXIT, "the hub to exit", || {
        Ok(!socket.exists())
    });
    if gone.is_err() {
        let pattern = ctx.home.fiber().to_string_lossy().into_owned();
        fakes::kill_matching(&pattern).map_err(|err| format!("killing the hub: {err}"))?;
        notes.push("hub did not exit".to_owned());
    }
    Ok(())
}

/// From just before the spawn to the pty output first holding `>`.
fn terminal_first_frame(ctx: &Ctx<'_>, notes: &mut Vec<String>) -> Result<Samples, String> {
    let terminal = Terminal::spawn(ctx.home, ctx.path.as_deref(), ctx.clock)?;
    let started = terminal.proc.spawned;
    let framed = terminal.wait_for(ctx.clock, started + READY, ">");
    let took = ms(ctx.clock, started);
    // The hub is up before the quit, so it idles out rather than starting
    // after the terminal has gone.
    let connected = framed.and_then(|()| hub_connected(ctx));
    let quit = quit(ctx, terminal, notes);
    connected?;
    quit?;
    Ok(vec![("terminal_first_frame_ms", took)])
}

/// The window starts once the first frame is drawn and the terminal holds
/// its hub connection.
fn terminal_idle(ctx: &Ctx<'_>, notes: &mut Vec<String>) -> Result<Samples, String> {
    let terminal = Terminal::spawn(ctx.home, ctx.path.as_deref(), ctx.clock)?;
    let pid = terminal.proc.pid();
    let measured = terminal
        .wait_for(ctx.clock, ctx.clock.now() + READY, ">")
        .and_then(|()| hub_connected(ctx))
        .and_then(|()| idle_window(ctx, pid, notes));
    let quit = quit(ctx, terminal, notes);
    let (switches, rss, _) = measured?;
    quit?;
    Ok(vec![
        ("terminal_idle_rss_kib", json!(rss)),
        ("terminal_idle_switches", json!(switches)),
    ])
}
