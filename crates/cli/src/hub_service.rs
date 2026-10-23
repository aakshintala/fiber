//! `fiber hub install`, `fiber hub uninstall`, the service stop top-level
//! `fiber stop` runs after its exchange, and the restart `fiber update`
//! calls (`docs/invocation.md`, "The hub"; `docs/releasing.md`,
//! "Updating"): the unit file and the service manager's calls.
//!
//! Install writes `hub.port`, then the unit file, then calls the manager.
//! The unit file records what the manager still has to load: when a manager
//! call fails after the file changed, the file returns to its previous
//! bytes, so the next install sees it changed again and repeats every call.

use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::commands::hub::{HubCommand, StopArgs};
use contract::shapes::Failure;
use serde_json::Value;
use support::clock::Parker;

use crate::hub_unit::{Manager, Service};
use crate::sessions::request_value;
use crate::{CliError, finish, write_line};

/// How often install asks launchd whether a booted-out service is gone.
const BOOTOUT_POLL: Duration = Duration::from_millis(100);

/// How long install waits for launchd to drop a booted-out service.
const BOOTOUT_DEADLINE: Duration = Duration::from_secs(10);

/// How long a manager invocation may run before the stop path kills it:
/// local `launchctl`/`systemctl` calls answer in milliseconds, so a wait
/// past this is a hang, not work.
pub(crate) const MANAGER_WAIT: Duration = Duration::from_secs(10);

/// The watchdog's re-check interval: a command that finishes between
/// slices still answers within one.
const WATCH_SLICE: Duration = Duration::from_millis(10);

/// How a manager command ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ran {
    pub(crate) success: bool,
    pub(crate) code: Option<i32>,
    pub(crate) stderr: String,
}

/// Runs the real program, with stdin and stdout closed and stderr kept.
pub(crate) fn system_run(program: &str, args: &[String]) -> io::Result<Ran> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    Ok(Ran {
        success: output.status.success(),
        code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// The stop path's bounded [`system_run`]: the child spawns in its
/// own process group, and past `MANAGER_WAIT` the whole group is killed
/// and reaped and the run fails `TimedOut`. A quiet child still answers
/// promptly: the watchdog re-checks every `WATCH_SLICE`, so a command
/// that finishes between slices returns within one. A background
/// descendant that outlives the direct child keeps the stderr pipe open:
/// the drain joins under the same bound, killing what remains of the
/// group first. Only the stop path runs through this bound; every other
/// manager call keeps `system_run` bit-identical, and fast manager calls
/// return the same `Ran` either way.
pub(crate) fn system_run_bounded(
    program: &str,
    args: &[String],
    clock: &dyn Clock,
) -> io::Result<Ran> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    // A chatty manager never blocks on a full stderr pipe: a thread
    // drains it while the watchdog waits on the child.
    let drained = {
        let mut stderr = child.stderr.take();
        thread::Builder::new()
            .name("manager-stderr".to_owned())
            .spawn(move || {
                let mut text = Vec::new();
                if let Some(stderr) = stderr.as_mut() {
                    stderr.read_to_end(&mut text).unwrap_or(0);
                }
                String::from_utf8_lossy(&text).into_owned()
            })
    };
    let drained = match drained {
        Ok(drained) => drained,
        Err(error) => {
            kill_group(child.id());
            // The reap runs for its side effect; after a failed spawn
            // there is nothing to report past the spawn error itself.
            #[allow(
                clippy::let_underscore_must_use,
                reason = "the reap runs for its side effect; its error changes nothing"
            )]
            let _ = child.wait();
            return Err(error);
        }
    };
    let deadline = clock.now() + MANAGER_WAIT;
    let parker = Parker::new();
    loop {
        let group = child.id();
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                kill_group(group);
                // The reap runs for its side effect; the wait's own
                // error is what the caller sees.
                #[allow(
                    clippy::let_underscore_must_use,
                    reason = "the reap runs for its side effect; its error changes nothing"
                )]
                let _ = child.wait();
                join_drained(drained, group, clock, deadline);
                return Err(error);
            }
        };
        if let Some(status) = status {
            // The child is gone, but a background descendant may still
            // hold the pipe: the drain joins under the bound, killing
            // what remains first.
            let stderr = join_drained(drained, group, clock, deadline);
            return Ok(Ran {
                success: status.success(),
                code: status.code(),
                stderr,
            });
        }
        if clock.now() >= deadline {
            kill_group(group);
            // The reap and the join both run for their side effects: the
            // kill above freed every pipe end, so both return at once.
            #[allow(
                clippy::let_underscore_must_use,
                reason = "the reap and the drain join run for their side effects; the timeout is what the caller sees"
            )]
            let _ = child.wait();
            join_drained(drained, group, clock, deadline);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("`{program}` did not finish in {} s", MANAGER_WAIT.as_secs()),
            ));
        }
        parker.park_polled(clock, Some(deadline), Some(WATCH_SLICE), 0);
    }
}

#[cfg(test)]
thread_local! {
    /// The drain test arms it, and `join_drained` runs it on its first
    /// unfinished pass: the runner reached its post-exit drain wait,
    /// not just any park at the bound. Test only: production has no
    /// seam there.
    static DRAIN_WAIT: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
}

/// Arms the one-shot drain hook before the next bounded run.
/// Test only: production has no seam there.
#[cfg(test)]
pub(crate) fn on_drain_wait(hook: impl FnOnce() + 'static) {
    DRAIN_WAIT.with(|slot| slot.set(Some(Box::new(hook))));
}

/// Joins the stderr drain under `deadline`: a background descendant that
/// outlives the direct child keeps the pipe open, so the join parks only
/// until the bound, kills the group that remains, then joins. Returns the
/// drained text, or empty when the drain thread itself failed.
fn join_drained(
    drained: thread::JoinHandle<String>,
    group: u32,
    clock: &dyn Clock,
    deadline: Instant,
) -> String {
    let parker = Parker::new();
    while !drained.is_finished() {
        // Tests only: the lingering-descendant case logs the drain
        // wait, on this same thread.
        #[cfg(test)]
        DRAIN_WAIT.with(|slot| {
            if let Some(hook) = slot.take() {
                hook();
            }
        });
        if clock.now() >= deadline {
            // A background descendant still holds the pipe: the kill
            // frees every end, so the join below returns at once.
            kill_group(group);
            break;
        }
        parker.park_polled(clock, Some(deadline), Some(WATCH_SLICE), 0);
    }
    drained.join().unwrap_or_default()
}

/// Kills `group` outright, ignoring every outcome: on the expiry path's
/// races the group may already be gone, and the reap after it still runs.
/// The guarded helper refuses an id of 1 or less before any syscall, so
/// no path here can reach the caller's own group or init.
fn kill_group(group: u32) {
    // A refused kill means the group is already gone or was never a run's;
    // the reap after it still runs.
    #[allow(
        clippy::let_underscore_must_use,
        reason = "a refused kill means the group is already gone; the reap after it still runs"
    )]
    let _ = support::group::signal(group, rustix::process::Signal::KILL);
}

/// `fiber hub install [--port <port>]` for this Fiber home and binary.
pub fn hub_install(port: Option<u16>, clock: &dyn Clock) -> i32 {
    let ran = system_service().and_then(|(home, service)| {
        let answers = hub_answers(&home, clock, doors::hub::CONNECT_DEADLINE);
        install(
            &service,
            &system_run,
            InstallRow {
                home: &home,
                port,
                clock,
                hub_answers: answers,
                out: &mut io::stdout(),
                write: &write_unit,
            },
        )
    });
    finish(ran)
}

/// `fiber hub uninstall` for this Fiber home.
pub fn hub_uninstall() -> i32 {
    let ran = system_service()
        .and_then(|(_, service)| uninstall(&service, &system_run, &mut io::stdout()));
    finish(ran)
}

/// Restarts `home`'s installed hub through its service manager: `Ok(true)`
/// once the manager restarted it, `Ok(false)` with nothing run when no
/// service is installed, so the caller signals the hub instead.
pub fn hub_restart(home: &Path) -> Result<bool, Failure> {
    restart_home(home).map_err(Failure::from)
}

/// [`hub_restart`] with the hub family's own error.
fn restart_home(home: &Path) -> Result<bool, CliError> {
    let exe = current_exe()?;
    let service = Service::locate(manager(), home, &exe, &|key| std::env::var_os(key))?;
    restart(&service, &system_run)
}

/// Fiber home and its service, from this process's environment.
fn system_service() -> Result<(std::path::PathBuf, Service), CliError> {
    let home = config::fiber_home_from_env()?;
    let exe = current_exe()?;
    let service = Service::locate(manager(), &home, &exe, &|key| std::env::var_os(key))?;
    Ok((home, service))
}

pub(crate) fn current_exe() -> Result<std::path::PathBuf, CliError> {
    std::env::current_exe().map_err(CliError::CurrentExe)
}

/// This platform's manager, for this process's user.
pub(crate) fn manager() -> Manager {
    Manager::current(rustix::process::getuid().as_raw())
}

/// Whether a hub answers `hub_hello` on `home`'s `run/hub` within `within`.
/// Never starts one.
pub(crate) fn hub_answers(home: &Path, clock: &dyn Clock, within: Duration) -> bool {
    let mut start = || Err(io::Error::new(io::ErrorKind::NotFound, "no hub runs"));
    doors::hub::connect_within(home, &mut start, clock, within).is_ok()
}

/// Writes the unit file's bytes. The write may leave the new bytes in
/// place while failing, when its rename succeeded before its directory
/// sync failed.
type UnitWrite = dyn Fn(&Path, &[u8]) -> Result<(), CliError>;

/// Runs a manager command to completion. The stop path passes a closure
/// over [`system_run_bounded`], which borrows the stop's clock, so the
/// bound lives on the alias and defaults to `'static` for every other call.
pub(crate) type ManagerRun<'a> = dyn Fn(&str, &[String]) -> io::Result<Ran> + 'a;

/// install's inputs, bundled so the signature stays small.
pub(crate) struct InstallRow<'a> {
    pub(crate) home: &'a Path,
    pub(crate) port: Option<u16>,
    pub(crate) clock: &'a dyn Clock,
    pub(crate) hub_answers: bool,
    pub(crate) out: &'a mut dyn Write,
    pub(crate) write: &'a UnitWrite,
}

/// The `id` of the `stop` command install sends a running hub.
const STOP_ID: &str = "c_stop";

/// Installs `service` listening on `port` as well as its socket, and loads
/// it. `hub_answers` says whether a hub already answers on `run/hub`.
/// `write` writes the unit file: a failed write restores like a failed
/// manager call.
pub(crate) fn install(
    service: &Service,
    run: &ManagerRun<'_>,
    row: InstallRow<'_>,
) -> Result<(), CliError> {
    let previous = read_unit(&service.unit)?;
    let rendered = service.render(row.port)?;
    let changed = previous.as_deref() != Some(rendered.as_bytes());
    if row.hub_answers && changed {
        // The installed hub takes over at once through the `run/` lock
        // handoff, so no wait follows the send. A second install of an
        // unchanged unit sends nothing, so the running hub keeps its pid.
        stop_running_hub(row.home, row.clock);
    }
    config::replace_global(row.home, "hub.port", row.port.map(Value::from))?;
    let loaded = match service.manager {
        Manager::Launchd { uid } => call(run, "launchctl", &print_args(service, uid))?.success,
        Manager::Systemd => previous.is_some(),
    };
    if row.hub_answers && !changed && !loaded {
        stop_running_hub(row.home, row.clock);
    }
    if changed && let Err(error) = (row.write)(&service.unit, rendered.as_bytes()) {
        return Err(restore_or_rerun(&service.unit, previous.as_deref(), error));
    }
    if let Err(error) = load(service, run, row.clock, loaded, changed) {
        return Err(
            match changed.then(|| restore(&service.unit, previous.as_deref())) {
                Some(Err(lost)) => CliError::UnitNotRestored {
                    cause: Box::new(error),
                    unit: service.unit.clone(),
                    lost: Box::new(lost),
                },
                Some(Ok(())) | None => CliError::RerunInstall {
                    cause: Box::new(error),
                },
            },
        );
    }
    write_line(
        row.out,
        &format!(
            "Installed the hub's login service: {}",
            service.unit.display()
        ),
    )?;
    Ok(())
}

/// Sends `stop` to the hub answering on `home`, if one still does, so the
/// installed hub takes over at once. Every failure proceeds silently: a
/// hub that vanished between the probe and the send needs nothing, and
/// the `run/` lock handoff covers a live hub.
pub(crate) fn stop_running_hub(home: &Path, clock: &dyn Clock) {
    let mut never = || Err(io::Error::new(io::ErrorKind::NotFound, "no hub runs"));
    let Ok((stream, _)) =
        doors::hub::connect_within(home, &mut never, clock, doors::hub::CONNECT_DEADLINE)
    else {
        return;
    };
    let mut read = BufReader::new(stream);
    drop(request_value(
        &mut read,
        STOP_ID,
        HubCommand::Stop(StopArgs { sessions: None }),
    ));
}

/// The manager calls after the unit file is written, per manager, whether
/// the service was loaded, and whether the file changed.
fn load(
    service: &Service,
    run: &ManagerRun<'_>,
    clock: &dyn Clock,
    loaded: bool,
    changed: bool,
) -> Result<(), CliError> {
    let unit = unit_name(service);
    match service.manager {
        Manager::Launchd { uid } => {
            let bootstrap = || {
                must(
                    run,
                    "launchctl",
                    &[
                        "bootstrap".to_owned(),
                        format!("gui/{uid}"),
                        service.unit.display().to_string(),
                    ],
                )
            };
            if !loaded {
                bootstrap()
            } else if changed {
                must(
                    run,
                    "launchctl",
                    &["bootout".to_owned(), target(service, uid)],
                )?;
                await_gone(service, uid, run, clock)?;
                bootstrap()
            } else {
                Ok(())
            }
        }
        Manager::Systemd if changed => {
            must(run, "systemctl", &user(&["daemon-reload"]))?;
            must(run, "systemctl", &user(&["enable", &unit]))?;
            must(run, "systemctl", &user(&["restart", &unit]))
        }
        Manager::Systemd => must(run, "systemctl", &user(&["enable", "--now", &unit])),
    }
}

/// Asks launchd every [`BOOTOUT_POLL`] until it no longer knows the
/// service, failing at [`BOOTOUT_DEADLINE`]: `bootout` can return before
/// the job is gone, and a `bootstrap` then fails.
fn await_gone(
    service: &Service,
    uid: u32,
    run: &ManagerRun<'_>,
    clock: &dyn Clock,
) -> Result<(), CliError> {
    let deadline = clock.now() + BOOTOUT_DEADLINE;
    loop {
        if !call(run, "launchctl", &print_args(service, uid))?.success {
            return Ok(());
        }
        if clock.now() >= deadline {
            return Err(CliError::LaunchdStillLoaded {
                name: service.name.clone(),
            });
        }
        clock.sleep(BOOTOUT_POLL);
    }
}

/// Unloads `service` and deletes its unit file. Running sessions carry on.
pub(crate) fn uninstall(
    service: &Service,
    run: &ManagerRun<'_>,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let removed = match service.manager {
        Manager::Launchd { uid } => {
            let loaded = call(run, "launchctl", &print_args(service, uid))?.success;
            if loaded {
                must(
                    run,
                    "launchctl",
                    &["bootout".to_owned(), target(service, uid)],
                )?;
            }
            delete_unit(&service.unit)? || loaded
        }
        Manager::Systemd => {
            if read_unit(&service.unit)?.is_some() {
                let unit = unit_name(service);
                must(run, "systemctl", &user(&["disable", "--now", &unit]))?;
                delete_unit(&service.unit)?;
                must(run, "systemctl", &user(&["daemon-reload"]))?;
                true
            } else {
                false
            }
        }
    };
    write_line(
        out,
        if removed {
            "Removed the hub's login service."
        } else {
            "The hub's login service is not installed."
        },
    )?;
    Ok(())
}

/// Restarts the installed service; `Ok(false)` with no call when its unit
/// file is absent.
pub(crate) fn restart(service: &Service, run: &ManagerRun<'_>) -> Result<bool, CliError> {
    if read_unit(&service.unit)?.is_none() {
        return Ok(false);
    }
    match service.manager {
        Manager::Launchd { uid } => must(
            run,
            "launchctl",
            &[
                "kickstart".to_owned(),
                "-k".to_owned(),
                target(service, uid),
            ],
        )?,
        Manager::Systemd => must(run, "systemctl", &user(&["restart", &unit_name(service)]))?,
    }
    Ok(true)
}

/// Stops `service`'s loaded manager job, keeping its unit file: true when a
/// loaded service was stopped. launchd skips a service it has not loaded;
/// systemd stops an active one and skips an inactive one.
pub(crate) fn stop_login_service(
    service: &Service,
    run: &ManagerRun<'_>,
    clock: &dyn Clock,
) -> Result<bool, CliError> {
    if read_unit(&service.unit)?.is_none() {
        return Ok(false);
    }
    match service.manager {
        Manager::Launchd { uid } => {
            if !call(run, "launchctl", &print_args(service, uid))?.success {
                return Ok(false);
            }
            must(
                run,
                "launchctl",
                &["bootout".to_owned(), target(service, uid)],
            )?;
            await_gone(service, uid, run, clock)?;
            Ok(true)
        }
        Manager::Systemd => {
            if !call(run, "systemctl", &user(&["is-active", &unit_name(service)]))?.success {
                return Ok(false);
            }
            must(run, "systemctl", &user(&["stop", &unit_name(service)]))?;
            Ok(true)
        }
    }
}

/// `gui/<uid>/<name>`.
fn target(service: &Service, uid: u32) -> String {
    format!("gui/{uid}/{}", service.name)
}

fn print_args(service: &Service, uid: u32) -> Vec<String> {
    vec!["print".to_owned(), target(service, uid)]
}

fn unit_name(service: &Service) -> String {
    format!("{}.service", service.name)
}

fn user(args: &[&str]) -> Vec<String> {
    std::iter::once("--user")
        .chain(args.iter().copied())
        .map(str::to_owned)
        .collect()
}

/// Runs a manager command; a missing program names what the installed hub
/// needs.
fn call(run: &ManagerRun<'_>, program: &str, args: &[String]) -> Result<Ran, CliError> {
    run(program, args).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            CliError::ManagerNotFound {
                program: program.to_owned(),
            }
        } else {
            CliError::ManagerRun {
                program: program.to_owned(),
                source: e,
            }
        }
    })
}

/// [`call`], failing when the command fails, naming it, its exit code and
/// the first line of its stderr.
fn must(run: &ManagerRun<'_>, program: &str, args: &[String]) -> Result<(), CliError> {
    let ran = call(run, program, args)?;
    if ran.success {
        return Ok(());
    }
    let said = match ran.stderr.lines().next().map(str::trim) {
        Some(first) if !first.is_empty() => Some(first.to_owned()),
        Some(_) | None => None,
    };
    Err(CliError::ManagerFailed {
        program: program.to_owned(),
        args: args.to_vec(),
        exit: ran.code,
        said,
    })
}

/// The unit file's bytes, `None` when there is none.
pub(crate) fn read_unit(unit: &Path) -> Result<Option<Vec<u8>>, CliError> {
    match fs::read(unit) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(CliError::UnitReadFailed {
            unit: unit.to_owned(),
            source: e,
        }),
    }
}

/// Writes the unit file through a temporary file and a rename, mode 0644.
fn write_unit(unit: &Path, bytes: &[u8]) -> Result<(), CliError> {
    config::write_atomic(unit, bytes, 0o644).map_err(CliError::UnitWriteFailed)
}

/// Deletes the unit file: true when there was one.
fn delete_unit(unit: &Path) -> Result<bool, CliError> {
    match fs::remove_file(unit) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(CliError::UnitDeleteFailed {
            unit: unit.to_owned(),
            source: e,
        }),
    }
}

/// Puts the unit file back as it was before install wrote it.
fn restore(unit: &Path, previous: Option<&[u8]>) -> Result<(), CliError> {
    match previous {
        Some(bytes) => write_unit(unit, bytes),
        None => delete_unit(unit).map(|_| ()),
    }
}

/// Restores `unit` to `previous` after a failed step, naming the rerun
/// that converges, or the uninstall when the restore itself fails.
fn restore_or_rerun(unit: &Path, previous: Option<&[u8]>, error: CliError) -> CliError {
    match restore(unit, previous) {
        Err(lost) => CliError::UnitNotRestored {
            cause: Box::new(error),
            unit: unit.to_owned(),
            lost: Box::new(lost),
        },
        Ok(()) => CliError::RerunInstall {
            cause: Box::new(error),
        },
    }
}

#[cfg(test)]
#[path = "hub_service_tests.rs"]
mod tests;
