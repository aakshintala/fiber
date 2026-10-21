//! `fiber hub install`, `fiber hub uninstall` and the restart `fiber update`
//! calls (`docs/invocation.md`, "The hub"; `docs/releasing.md`,
//! "Updating"): the unit file and the service manager's calls.
//!
//! Install writes `hub.port`, then the unit file, then calls the manager.
//! The unit file records what the manager still has to load: when a manager
//! call fails after the file changed, the file returns to its previous
//! bytes, so the next install sees it changed again and repeats every call.

use std::fs;
use std::io::{self, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::commands::hub::{HubCommand, NoArgs};
use contract::shapes::Failure;
use serde_json::Value;

use crate::hub_unit::{Manager, Service};
use crate::sessions::request_value;
use crate::{failed, finish, write_line};

/// How often install asks launchd whether a booted-out service is gone.
const BOOTOUT_POLL: Duration = Duration::from_millis(100);

/// How long install waits for launchd to drop a booted-out service.
const BOOTOUT_DEADLINE: Duration = Duration::from_secs(10);

/// What a failed install adds, so the person knows a rerun converges.
const RERUN: &str =
    "Run fiber hub install again, or fiber hub uninstall to remove what was written.";

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
    let exe = current_exe()?;
    let service = Service::locate(manager(), home, &exe, &|key| std::env::var_os(key))?;
    restart(&service, &system_run)
}

/// Fiber home and its service, from this process's environment.
fn system_service() -> Result<(std::path::PathBuf, Service), Failure> {
    let home = config::fiber_home_from_env().map_err(|e| failed(e.code(), e))?;
    let exe = current_exe()?;
    let service = Service::locate(manager(), &home, &exe, &|key| std::env::var_os(key))?;
    Ok((home, service))
}

fn current_exe() -> Result<std::path::PathBuf, Failure> {
    std::env::current_exe().map_err(|e| {
        failed(
            ErrorCode::IoFailed,
            format!("the running binary's path: {e}"),
        )
    })
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
type UnitWrite = dyn Fn(&Path, &[u8]) -> Result<(), Failure>;

/// Runs a manager command to completion.
type ManagerRun = dyn Fn(&str, &[String]) -> io::Result<Ran>;

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
    run: &ManagerRun,
    row: InstallRow<'_>,
) -> Result<(), Failure> {
    let previous = read_unit(&service.unit)?;
    let rendered = service.render(row.port)?;
    let changed = previous.as_deref() != Some(rendered.as_bytes());
    if row.hub_answers && changed {
        // The installed hub takes over at once through the `run/` lock
        // handoff, so no wait follows the send. A second install of an
        // unchanged unit sends nothing, so the running hub keeps its pid.
        stop_running_hub(row.home, row.clock);
    }
    config::replace_global(row.home, "hub.port", row.port.map(Value::from))
        .map_err(|e| failed(e.code(), e))?;
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
        let message = match changed.then(|| restore(&service.unit, previous.as_deref())) {
            Some(Err(lost)) => format!(
                "{} The unit file {} could not be put back ({}); run fiber hub uninstall.",
                error.message,
                service.unit.display(),
                lost.message
            ),
            Some(Ok(())) | None => format!("{} {RERUN}", error.message),
        };
        return Err(failed(error.code, message));
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
        HubCommand::Stop(NoArgs {}),
    ));
}

/// The manager calls after the unit file is written, per manager, whether
/// the service was loaded, and whether the file changed.
fn load(
    service: &Service,
    run: &ManagerRun,
    clock: &dyn Clock,
    loaded: bool,
    changed: bool,
) -> Result<(), Failure> {
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
    run: &ManagerRun,
    clock: &dyn Clock,
) -> Result<(), Failure> {
    let deadline = clock.now() + BOOTOUT_DEADLINE;
    loop {
        if !call(run, "launchctl", &print_args(service, uid))?.success {
            return Ok(());
        }
        if clock.now() >= deadline {
            return Err(failed(
                ErrorCode::IoFailed,
                format!(
                    "launchd still had {} loaded 10 s after `launchctl bootout`.",
                    service.name
                ),
            ));
        }
        clock.sleep(BOOTOUT_POLL);
    }
}

/// Unloads `service` and deletes its unit file. Running sessions carry on.
pub(crate) fn uninstall(
    service: &Service,
    run: &ManagerRun,
    out: &mut dyn Write,
) -> Result<(), Failure> {
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
    )
}

/// Restarts the installed service; `Ok(false)` with no call when its unit
/// file is absent.
pub(crate) fn restart(service: &Service, run: &ManagerRun) -> Result<bool, Failure> {
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
fn call(run: &ManagerRun, program: &str, args: &[String]) -> Result<Ran, Failure> {
    run(program, args).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            let needs = if program == "launchctl" {
                "launchd"
            } else {
                "systemd's user instance"
            };
            failed(
                ErrorCode::IoFailed,
                format!("`{program}` was not found; an installed hub needs {needs}."),
            )
        } else {
            failed(ErrorCode::IoFailed, format!("running `{program}`: {e}"))
        }
    })
}

/// [`call`], failing when the command fails, naming it, its exit code and
/// the first line of its stderr.
fn must(run: &ManagerRun, program: &str, args: &[String]) -> Result<(), Failure> {
    let ran = call(run, program, args)?;
    if ran.success {
        return Ok(());
    }
    let code = ran.code.map_or_else(
        || "no exit code".to_owned(),
        |code| format!("exit code {code}"),
    );
    let said = match ran.stderr.lines().next().map(str::trim) {
        Some(first) if !first.is_empty() => format!(": {first}"),
        Some(_) | None => String::new(),
    };
    Err(failed(
        ErrorCode::IoFailed,
        format!("`{program} {}` failed with {code}{said}.", args.join(" ")),
    ))
}

/// The unit file's bytes, `None` when there is none.
fn read_unit(unit: &Path) -> Result<Option<Vec<u8>>, Failure> {
    match fs::read(unit) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(failed(
            ErrorCode::IoFailed,
            format!("reading {}: {e}", unit.display()),
        )),
    }
}

/// Writes the unit file through a temporary file and a rename, mode 0644.
fn write_unit(unit: &Path, bytes: &[u8]) -> Result<(), Failure> {
    config::write_atomic(unit, bytes, 0o644).map_err(|e| failed(ErrorCode::IoFailed, e))
}

/// Deletes the unit file: true when there was one.
fn delete_unit(unit: &Path) -> Result<bool, Failure> {
    match fs::remove_file(unit) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(failed(
            ErrorCode::IoFailed,
            format!("deleting {}: {e}", unit.display()),
        )),
    }
}

/// Puts the unit file back as it was before install wrote it.
fn restore(unit: &Path, previous: Option<&[u8]>) -> Result<(), Failure> {
    match previous {
        Some(bytes) => write_unit(unit, bytes),
        None => delete_unit(unit).map(|_| ()),
    }
}

/// Restores `unit` to `previous` after a failed step, naming the rerun
/// that converges, or the uninstall when the restore itself fails.
fn restore_or_rerun(unit: &Path, previous: Option<&[u8]>, error: Failure) -> Failure {
    let message = match restore(unit, previous) {
        Err(lost) => format!(
            "{} The unit file {} could not be put back ({}); run fiber hub uninstall.",
            error.message,
            unit.display(),
            lost.message
        ),
        Ok(()) => format!("{} {RERUN}", error.message),
    };
    failed(error.code, message)
}

#[cfg(test)]
#[path = "hub_service_tests.rs"]
mod tests;
