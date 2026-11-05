//! The keeper's peak-RSS workload (`docs/performance.md`, "Budgets"): a
//! session running one long shell job, measuring the keeper process's peak
//! RSS on Linux x86_64. The keeper is the session's own process, started in
//! a session of its own, so the workload finds it by its `keeper --id <id>`
//! command line and reads the kernel's peak RSS for its pid.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

use contract::clock::Clock;
use fakes::Response;
use serde_json::json;

use crate::busy::{self, expect};
use crate::home::Home;
use crate::idle::{self, Ctx, Samples, Workload};
use crate::linux::{self, Counts};
use crate::run::{self, Session};

/// The workloads: the keeper's peak RSS with one running job.
pub(crate) const WORKLOADS: [Workload; 1] = [Workload {
    name: "keeper",
    timing: false,
    run: keeper_peak,
}];

/// What starts the session's turn: it runs the one shell job, then ends.
const KEEPER_PROMPT: &str = "start the long job";

/// How long the shell job runs: it outlasts every pre-read wait with
/// margin, so the job still runs at the VmHWM read.
const SLEEP_SECS: u64 = 180;

/// What the session runs in the background: the workload the budget covers.
fn job_command() -> String {
    format!("sleep {SLEEP_SECS}")
}

/// The provider's replies: one background shell call running the long job,
/// then the closing text.
fn keeper_script() -> Vec<Response> {
    let arguments = json!({"command": job_command(), "run_in_background": true}).to_string();
    vec![
        busy::call_reply("call_job", "shell", &arguments, KEEPER_PROMPT.len()),
        busy::text_reply("the job is running", KEEPER_PROMPT.len() + arguments.len()),
    ]
}

/// The keeper's `/proc/<pid>/status` text, the VmHWM read's input.
fn status_text(keeper: u32) -> Result<String, String> {
    let path = Path::new("/proc").join(keeper.to_string()).join("status");
    fs::read_to_string(&path).map_err(|err| format!("reading {}: {err}", path.display()))
}

/// The one keeper pid: zero matches keep polling to `within`'s expiry, an
/// error naming the wait, and two or more matches error at once with no
/// retry on ambiguity.
fn poll_keeper(
    clock: &dyn Clock,
    within: Duration,
    mut find_keeper: impl FnMut() -> Result<Vec<u32>, String>,
) -> Result<u32, String> {
    let keeper = Cell::new(None);
    run::poll(
        clock,
        within,
        "the keeper to start",
        || match find_keeper()?.as_slice() {
            [] => Ok(false),
            [pid] => {
                keeper.set(Some(*pid));
                Ok(true)
            }
            pids => Err(format!("more than one keeper matches: {pids:?}")),
        },
    )?;
    keeper
        .get()
        .ok_or_else(|| "the keeper poll ended with no keeper".to_owned())
}

/// The keeper's peak RSS: settles its threads, then reads the kernel's peak
/// resident set size from its status text. A missing field, or a keeper
/// gone during the wait, is an error, never 0: a zero would pass an exact
/// budget.
fn keeper_rss_kib(
    clock: &dyn Clock,
    within: Duration,
    mut read: impl FnMut() -> Result<BTreeMap<u32, Counts>, String>,
    read_status: impl FnOnce() -> Result<String, String>,
) -> Result<Samples, String> {
    idle::settle(clock, within, &mut read)?;
    let rss = linux::vm_hwm_kib(&read_status()?)?;
    Ok(vec![("keeper_rss_kib", json!(rss))])
}

/// Ends the session gracefully: `close` with `now` starts a shutdown
/// instead (`docs/invocation.md`, "Shutdown"), and the keeper shuts down
/// with its session (`docs/tools.md`, "The keeper").
fn close_now(ctx: &Ctx<'_>, home: &Home, id: &str) -> Result<(), String> {
    let mut client = crate::resume::subscribe(ctx, home, id)?;
    client.send(&crate::resume::command(
        "c_close",
        "close",
        &json!({"now": true}),
    ))
}

/// Waits for the keeper's own pid to exit once its session shut down. On
/// expiry SIGTERMs the pid and errs naming it, so the failure says the
/// keeper did not exit instead of leaving `Home::finish` to report an
/// anonymous survivor.
fn wait_keeper(keeper: u32) -> Result<(), String> {
    if fakes::pids_exit(&[keeper], fakes::Deadline::after(run::STOP)) {
        return Ok(());
    }
    fakes::kill_pid(keeper, "TERM").map_err(|err| format!("signalling keeper {keeper}: {err}"))?;
    Err(format!(
        "keeper {keeper} did not exit after its session shut down"
    ))
}

/// The standing shell allow-rule for the long job: the session runs it
/// without asking, the shape `crates/main/tests/keeper.rs` pins.
fn write_rule(home: &Home) -> Result<(), String> {
    let rule = json!({"decision": "allow", "tool": "shell", "prefix": job_command()});
    let path = home.home().join("rules");
    fs::write(&path, format!("{rule}\n"))
        .map_err(|err| format!("writing {}: {err}", path.display()))
}

/// One session running the long job: waits for the job to start (the keeper
/// spawned it, so the keeper exists), polls for exactly one keeper pid,
/// settles the keeper's threads, and reads the kernel's peak RSS for the
/// keeper pid while the job still runs. Then shuts the session down
/// gracefully instead of waiting out the sleep: the keeper is not in the
/// session's process group, so `Proc::stop` cannot reap it.
fn keeper_peak(ctx: &Ctx<'_>, notes: &mut Vec<String>) -> Result<Samples, String> {
    let script = keeper_script();
    busy::in_home(ctx.home.child(script)?, |home| {
        write_rule(home)?;
        let id = doors::mint("s_");
        let session = Session::spawn(
            &mut busy::session(ctx, home, &id, &["--prompt", KEEPER_PROMPT]),
            ctx.clock,
        )?;
        let pattern = format!("keeper --id {id}");
        let measured = (|| {
            busy::until_stdout(ctx, &session, "job_started", busy::READY)?;
            let keeper = poll_keeper(ctx.clock, busy::READY, || {
                fakes::matching(&pattern).map_err(|err| format!("listing keeper processes: {err}"))
            })?;
            keeper_rss_kib(
                ctx.clock,
                busy::READY,
                || linux::threads(keeper),
                || status_text(keeper),
            )
            .map(|samples| (keeper, samples))
        })();
        let mut proc = session.proc;
        let shutdown = measured.and_then(|(keeper, samples)| {
            close_now(ctx, home, &id)?;
            if !proc.exits(ctx.clock, busy::READY)? {
                notes.push("the session did not exit on close".to_owned());
            }
            wait_keeper(keeper)?;
            Ok(samples)
        });
        let stopped = proc.stop(ctx.clock);
        let samples = shutdown?;
        stopped?;
        let lines = busy::read_log(home, &id)?;
        expect(
            notes,
            "completed tool calls",
            busy::count(&lines, "tool_call_completed", "status", "completed"),
            1,
        );
        Ok(samples)
    })
}

#[cfg(test)]
#[path = "keeper_tests.rs"]
mod tests;
