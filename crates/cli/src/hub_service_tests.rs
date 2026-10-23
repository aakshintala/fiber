//! Install, uninstall and restart against a recording stub service
//! manager: the exact calls per row of the install table, the unit file's
//! bytes, the restore after a failed call, and the convergence of a rerun.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::cell::RefCell;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::{
    InstallRow, MANAGER_WAIT, Ran, current_exe, hub_answers, hub_install, hub_restart,
    hub_uninstall, install, must, read_unit, restart, stop_login_service, system_run,
    system_run_bounded, uninstall,
};
use crate::CliError;
use crate::error::RERUN;
use crate::hub_unit::{Manager, Service, name};

/// One named deadline per blocking call.
const DEADLINE: Duration = Duration::from_secs(10);

const UID: u32 = 501;

type Script = Box<dyn FnMut(&str) -> io::Result<Ran>>;

/// The command lines one recording manager ran.
type Calls = Rc<RefCell<Vec<String>>>;

/// Runs a service manager's command to completion.
type RunCommand = Box<super::ManagerRun<'static>>;

/// A manager that records each command line in `calls` and answers it from
/// `script`.
fn recording(script: impl FnMut(&str) -> io::Result<Ran> + 'static, calls: &Calls) -> RunCommand {
    let calls = Rc::clone(calls);
    let script: RefCell<Script> = RefCell::new(Box::new(script));
    Box::new(move |program, args| {
        let line = format!("{program} {}", args.join(" "));
        calls.borrow_mut().push(line.clone());
        (script.borrow_mut())(&line)
    })
}

/// A service manager that records each command line and answers it from a
/// script: the closure in `run` runs the manager, `calls` replays what ran.
struct Stub {
    calls: Calls,
    run: RunCommand,
}

impl Stub {
    fn new(script: impl FnMut(&str) -> io::Result<Ran> + 'static) -> Self {
        let calls: Calls = Default::default();
        let run = recording(script, &calls);
        Self { calls, run }
    }

    /// Every command succeeds.
    fn ok() -> Self {
        Self::new(|_| Ok(ok()))
    }

    /// `launchctl print` fails (not loaded); everything else succeeds.
    fn unloaded() -> Self {
        Self::new(|line| Ok(if line.contains(" print ") { no() } else { ok() }))
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

fn ok() -> Ran {
    Ran {
        success: true,
        code: Some(0),
        stderr: String::new(),
    }
}

fn no() -> Ran {
    Ran {
        success: false,
        code: Some(113),
        stderr: "Could not find service\n".to_owned(),
    }
}

fn boom() -> Ran {
    Ran {
        success: false,
        code: Some(5),
        stderr: "boom\nsecond line\n".to_owned(),
    }
}

struct Setup {
    dir: fakes::TempDir,
    clock: Arc<FakeClock>,
}

impl Setup {
    fn new() -> Self {
        let dir = fakes::TempDir::new("cli-hub-service");
        fs::create_dir_all(dir.path().join("home")).unwrap();
        fs::create_dir_all(dir.path().join("units")).unwrap();
        Self {
            dir,
            clock: FakeClock::new(),
        }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn service(&self, manager: Manager) -> Service {
        let home = self.home();
        let name = name(&home).unwrap();
        let file = match manager {
            Manager::Launchd { .. } => format!("{name}.plist"),
            Manager::Systemd => format!("{name}.service"),
        };
        Service {
            manager,
            name,
            unit: self.dir.path().join("units").join(file),
            exe: PathBuf::from("/opt/fiber/bin/fiber"),
            home,
        }
    }

    fn install(
        &self,
        service: &Service,
        port: Option<u16>,
        stub: &Stub,
        answers: bool,
    ) -> (Result<(), CliError>, String) {
        let mut out = Vec::new();
        let result = install(
            service,
            &stub.run,
            InstallRow {
                home: &self.home(),
                port,
                clock: &*self.clock,
                hub_answers: answers,
                out: &mut out,
                write: &super::write_unit,
            },
        );
        (result, String::from_utf8(out).unwrap())
    }

    fn port(&self) -> Option<Value> {
        config::get_global(&self.home(), "hub.port").unwrap()
    }
}

fn launchd() -> Manager {
    Manager::Launchd { uid: UID }
}

fn print(service: &Service) -> String {
    format!("launchctl print gui/{UID}/{}", service.name)
}

fn bootout(service: &Service) -> String {
    format!("launchctl bootout gui/{UID}/{}", service.name)
}

fn bootstrap(service: &Service) -> String {
    format!("launchctl bootstrap gui/{UID} {}", service.unit.display())
}

fn systemctl(args: &str) -> String {
    format!("systemctl --user {args}")
}

fn changed_row(service: &Service) -> Vec<String> {
    let unit = format!("{}.service", service.name);
    vec![
        systemctl("daemon-reload"),
        systemctl(&format!("enable {unit}")),
        systemctl(&format!("restart {unit}")),
    ]
}

fn installed_line(service: &Service) -> String {
    format!(
        "Installed the hub's login service: {}\n",
        service.unit.display()
    )
}

#[test]
fn launchd_not_loaded_writes_the_plist_and_bootstraps() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    let stub = Stub::unloaded();
    let (result, out) = setup.install(&service, Some(4040), &stub, false);
    result.unwrap();
    assert_eq!(stub.calls(), [print(&service), bootstrap(&service)]);
    assert_eq!(
        fs::read_to_string(&service.unit).unwrap(),
        service.render(Some(4040)).unwrap()
    );
    assert_eq!(out, installed_line(&service));
    assert_eq!(setup.port(), Some(json!(4040)));
}

#[test]
fn launchd_not_loaded_bootstraps_even_when_the_plist_is_unchanged() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, service.render(None).unwrap()).unwrap();
    let stub = Stub::unloaded();
    setup.install(&service, None, &stub, false).0.unwrap();
    assert_eq!(stub.calls(), [print(&service), bootstrap(&service)]);
}

#[test]
fn a_second_identical_launchd_install_only_asks() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    setup
        .install(&service, None, &Stub::unloaded(), false)
        .0
        .unwrap();
    let before = fs::read(&service.unit).unwrap();
    let stub = Stub::ok();
    let (result, out) = setup.install(&service, None, &stub, false);
    result.unwrap();
    assert_eq!(stub.calls(), [print(&service)]);
    assert_eq!(fs::read(&service.unit).unwrap(), before);
    assert_eq!(out, installed_line(&service));
}

#[test]
fn launchd_loaded_and_changed_boots_out_waits_until_gone_then_bootstraps() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, service.render(None).unwrap()).unwrap();
    // Loaded before; after bootout launchd still knows it for two asks.
    let mut prints = 0;
    let stub = Stub::new(move |line| {
        if line.contains(" print ") {
            prints += 1;
            return Ok(if prints <= 3 { ok() } else { no() });
        }
        Ok(ok())
    });
    setup.install(&service, Some(4040), &stub, false).0.unwrap();
    assert_eq!(
        stub.calls(),
        [
            print(&service),
            bootout(&service),
            print(&service),
            print(&service),
            print(&service),
            bootstrap(&service),
        ]
    );
    assert_eq!(
        setup.clock.now().duration_since(setup.clock.origin()),
        Duration::from_millis(200),
        "one poll interval between each ask"
    );
    assert_eq!(
        fs::read_to_string(&service.unit).unwrap(),
        service.render(Some(4040)).unwrap()
    );
}

#[test]
fn launchd_still_loaded_10_s_after_bootout_fails_and_restores_the_plist() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    let previous = service.render(None).unwrap();
    fs::write(&service.unit, &previous).unwrap();
    let stub = Stub::ok();
    let (result, out) = setup.install(&service, Some(4040), &stub, false);
    let error = result.unwrap_err();
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert!(
        matches!(
            &error,
            CliError::RerunInstall { cause } if matches!(**cause, CliError::LaunchdStillLoaded { .. })
        ),
        "{error}"
    );
    assert!(error.to_string().ends_with(RERUN), "{error}");
    // Asked at 0, 100 ms, ..., 10 s after the bootout.
    let calls = stub.calls();
    assert_eq!(calls.get(1), Some(&bootout(&service)));
    assert_eq!(calls.len(), 2 + 101);
    assert!(calls.iter().skip(2).all(|line| *line == print(&service)));
    assert_eq!(fs::read_to_string(&service.unit).unwrap(), previous);
    assert_eq!(out, "");
}

#[test]
fn launchd_bootstrap_failing_after_bootout_restores_and_the_rerun_bootstraps() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    let previous = service.render(None).unwrap();
    fs::write(&service.unit, &previous).unwrap();
    let mut prints = 0;
    let stub = Stub::new(move |line| {
        if line.contains(" print ") {
            prints += 1;
            return Ok(if prints == 1 { ok() } else { no() });
        }
        Ok(if line.contains(" bootstrap ") {
            boom()
        } else {
            ok()
        })
    });
    let error = setup
        .install(&service, Some(4040), &stub, false)
        .0
        .unwrap_err();
    assert!(
        matches!(
            &error,
            CliError::RerunInstall { cause } if matches!(**cause, CliError::ManagerFailed { .. })
        ),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        format!(
            "`launchctl bootstrap gui/{UID} {}` failed with exit code 5: boom. {RERUN}",
            service.unit.display()
        )
    );
    assert_eq!(fs::read_to_string(&service.unit).unwrap(), previous);

    let rerun = Stub::unloaded();
    setup
        .install(&service, Some(4040), &rerun, false)
        .0
        .unwrap();
    assert_eq!(rerun.calls(), [print(&service), bootstrap(&service)]);
    assert_eq!(
        fs::read_to_string(&service.unit).unwrap(),
        service.render(Some(4040)).unwrap()
    );
}

#[test]
fn a_write_that_fails_after_the_rename_restores_and_the_rerun_takes_the_changed_row() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    setup.install(&service, None, &Stub::ok(), false).0.unwrap();
    let previous = fs::read(&service.unit).unwrap();
    let rendered = service.render(Some(4040)).unwrap();
    assert_ne!(previous, rendered.as_bytes());

    // The write leaves the new bytes in place (the rename succeeded)
    // then fails (its directory sync failed), like `write_atomic` after
    // the rename.
    let failing = |path: &Path, bytes: &[u8]| -> Result<(), CliError> {
        fs::write(path, bytes).unwrap();
        Err(CliError::UnitWriteFailed(config::ConfigError::Unsynced {
            file: path.to_owned(),
            source: io::Error::other("directory sync failed"),
        }))
    };
    let mut out = Vec::new();
    let error = install(
        &service,
        &Stub::ok().run,
        InstallRow {
            home: &setup.home(),
            port: Some(4040),
            clock: &*setup.clock,
            hub_answers: false,
            out: &mut out,
            write: &failing,
        },
    )
    .unwrap_err();
    assert!(
        matches!(
            &error,
            CliError::RerunInstall { cause } if matches!(**cause, CliError::UnitWriteFailed(_))
        ),
        "{error}"
    );
    assert!(error.to_string().ends_with(RERUN), "{}", error.to_string());
    assert!(
        error.to_string().contains("directory sync failed"),
        "{}",
        error.to_string()
    );
    assert_eq!(fs::read(&service.unit).unwrap(), previous);
    assert!(out.is_empty(), "{out:?}");

    let rerun = Stub::ok();
    setup
        .install(&service, Some(4040), &rerun, false)
        .0
        .unwrap();
    assert_eq!(rerun.calls(), changed_row(&service));
    assert_eq!(fs::read_to_string(&service.unit).unwrap(), rendered);
}

#[test]
fn systemd_fresh_install_reloads_enables_and_restarts() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    let stub = Stub::ok();
    let (result, out) = setup.install(&service, None, &stub, false);
    result.unwrap();
    assert_eq!(stub.calls(), changed_row(&service));
    assert_eq!(
        fs::read_to_string(&service.unit).unwrap(),
        service.render(None).unwrap()
    );
    assert_eq!(out, installed_line(&service));
}

#[test]
fn a_second_identical_systemd_install_enables_now_and_restarts_nothing() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    setup
        .install(&service, Some(4040), &Stub::ok(), false)
        .0
        .unwrap();
    let stub = Stub::ok();
    setup.install(&service, Some(4040), &stub, false).0.unwrap();
    assert_eq!(
        stub.calls(),
        [systemctl(&format!("enable --now {}.service", service.name))]
    );
}

#[test]
fn a_port_only_change_whose_restart_fails_is_restarted_by_the_rerun() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    setup.install(&service, None, &Stub::ok(), false).0.unwrap();
    let previous = fs::read(&service.unit).unwrap();

    let stub = Stub::new(|line| {
        Ok(if line.contains(" restart ") {
            boom()
        } else {
            ok()
        })
    });
    let error = setup
        .install(&service, Some(4040), &stub, false)
        .0
        .unwrap_err();
    assert!(
        matches!(
            &error,
            CliError::RerunInstall { cause } if matches!(**cause, CliError::ManagerFailed { .. })
        ),
        "{error}"
    );
    assert_eq!(
        error.to_string(),
        format!(
            "`systemctl --user restart {}.service` failed with exit code 5: boom. {RERUN}",
            service.name
        )
    );
    assert_eq!(fs::read(&service.unit).unwrap(), previous);
    assert_eq!(
        setup.port(),
        Some(json!(4040)),
        "hub.port is not rolled back"
    );

    let rerun = Stub::ok();
    setup
        .install(&service, Some(4040), &rerun, false)
        .0
        .unwrap();
    assert_eq!(rerun.calls(), changed_row(&service));
}

#[test]
fn a_failed_daemon_reload_on_a_fresh_install_removes_the_unit_and_the_rerun_reloads() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    let stub = Stub::new(|line| {
        Ok(if line.contains("daemon-reload") {
            boom()
        } else {
            ok()
        })
    });
    let error = setup.install(&service, None, &stub, false).0.unwrap_err();
    assert!(
        error.to_string().contains("daemon-reload"),
        "{}",
        error.to_string()
    );
    assert!(!service.unit.exists());
    assert_eq!(stub.calls(), [systemctl("daemon-reload")]);

    let rerun = Stub::ok();
    setup.install(&service, None, &rerun, false).0.unwrap();
    assert_eq!(rerun.calls(), changed_row(&service));
}

#[test]
fn an_unchanged_unit_is_left_as_it_is_when_its_call_fails() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    setup.install(&service, None, &Stub::ok(), false).0.unwrap();
    let before = fs::read(&service.unit).unwrap();
    let stub = Stub::new(|_| Ok(boom()));
    let error = setup.install(&service, None, &stub, false).0.unwrap_err();
    assert!(
        error.to_string().contains("enable --now"),
        "{}",
        error.to_string()
    );
    assert_eq!(fs::read(&service.unit).unwrap(), before);
}

#[test]
fn a_restore_that_fails_names_the_unit_file_and_says_to_uninstall() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    let units = setup.dir.path().join("units");
    let stub = Stub::new(move |_| {
        // The unit is written; the directory then refuses the restore's
        // removal.
        fs::set_permissions(&units, fs::Permissions::from_mode(0o555)).unwrap();
        Ok(boom())
    });
    let error = setup.install(&service, None, &stub, false).0.unwrap_err();
    fs::set_permissions(
        setup.dir.path().join("units"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(
        matches!(&error, CliError::UnitNotRestored { cause, .. } if matches!(**cause, CliError::ManagerFailed { .. })),
        "{error}"
    );
    assert!(
        error.to_string().contains(&format!(
            "The unit file {} could not be put back",
            service.unit.display()
        )),
        "{}",
        error.to_string()
    );
    assert!(
        error.to_string().contains("run fiber hub uninstall"),
        "{}",
        error.to_string()
    );
    assert!(!error.to_string().contains(RERUN), "{}", error.to_string());
}

#[test]
fn a_missing_service_manager_says_what_the_installed_hub_needs() {
    for (manager, program, sentence) in [
        (
            launchd(),
            "launchctl",
            "`launchctl` was not found; an installed hub needs launchd.",
        ),
        (
            Manager::Systemd,
            "systemctl",
            "`systemctl` was not found; an installed hub needs systemd's user instance.",
        ),
    ] {
        let setup = Setup::new();
        let service = setup.service(manager);
        let stub = Stub::new(|_| Err(io::Error::new(io::ErrorKind::NotFound, "no such file")));
        let error = setup.install(&service, None, &stub, false).0.unwrap_err();
        assert_eq!(error.code(), ErrorCode::IoFailed);
        // launchd asks before it writes, so its error is bare; systemd's
        // comes from the load after the write.
        let case = match &error {
            CliError::RerunInstall { cause } => &**cause,
            other => other,
        };
        assert!(
            matches!(case, CliError::ManagerNotFound { program: p } if p == program),
            "{error}"
        );
        assert!(error.to_string().starts_with(sentence), "{error}");
        assert!(!service.unit.exists());
    }
}

#[test]
fn another_spawn_failure_names_the_program() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    let stub = Stub::new(|_| Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")));
    let error = setup.install(&service, None, &stub, false).0.unwrap_err();
    assert!(
        matches!(
            &error,
            CliError::RerunInstall { cause }
                if matches!(&**cause, CliError::ManagerRun { program, .. } if program == "systemctl")
        ),
        "{error}"
    );
    assert!(
        error.to_string().starts_with("running `systemctl`: denied"),
        "{error}"
    );
}

#[test]
fn a_blank_first_stderr_line_adds_no_quote_to_the_error() {
    // An empty stderr takes the `None` arm, a whitespace-only first line
    // trims to empty: neither names stderr in the error.
    for stderr in [String::new(), "   \nsecond line\n".to_owned()] {
        let stub = Stub::new({
            let stderr = stderr.clone();
            move |_| {
                Ok(Ran {
                    success: false,
                    code: Some(5),
                    stderr: stderr.clone(),
                })
            }
        });
        let error = must(
            &stub.run,
            "launchctl",
            &["bootstrap".to_owned(), "gui/501/x".to_owned()],
        )
        .unwrap_err();
        assert_eq!(error.code(), ErrorCode::IoFailed);
        assert!(
            matches!(&error, CliError::ManagerFailed { said: None, .. }),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            "`launchctl bootstrap gui/501/x` failed with exit code 5.",
            "stderr {stderr:?}"
        );
    }
}

#[test]
fn current_exe_is_the_running_binary() {
    assert_eq!(
        current_exe().unwrap(),
        std::env::current_exe().unwrap(),
        "the hub restart runs this binary"
    );
}

#[test]
fn install_with_a_vanished_hub_still_installs() {
    // No socket: the hub vanished between the probe and the send, so the
    // send fails and the install carries on through the manager calls.
    let setup = Setup::new();
    let service = setup.service(launchd());
    let stub = Stub::unloaded();
    let (result, out) = setup.install(&service, None, &stub, true);
    result.unwrap();
    assert_eq!(out, installed_line(&service));
    assert_eq!(stub.calls(), [print(&service), bootstrap(&service)]);
}

#[test]
fn a_hub_answering_while_the_service_is_loaded_prints_no_takeover_line() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, service.render(None).unwrap()).unwrap();
    let (result, out) = setup.install(&service, None, &Stub::ok(), true);
    result.unwrap();
    assert_eq!(out, installed_line(&service));

    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    setup.install(&service, None, &Stub::ok(), false).0.unwrap();
    let (result, out) = setup.install(&service, None, &Stub::ok(), true);
    result.unwrap();
    assert_eq!(out, installed_line(&service));
}

/// A hub on `home`'s `run/hub` that answers every command with
/// `command_accepted` and logs each command line in `events` before it
/// answers, so the log orders the stop against the manager calls.
/// Answering every connection keeps the install off the injected clock:
/// an extra stop (such as the flipped `!changed` condition's) is
/// answered too, so the install returns and the assertions fail instead
/// of hanging. Dropping the shutdown sender ends the loop, so no join
/// ever waits on a connection that never comes.
fn stopping_hub(
    home: &Path,
    events: Arc<std::sync::Mutex<Vec<String>>>,
) -> (std::thread::JoinHandle<()>, std::sync::mpsc::Sender<()>) {
    fs::create_dir_all(home.join("run")).unwrap();
    let listener = UnixListener::bind(home.join("run/hub")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        loop {
            match stop_rx.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                _ => return,
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    // The accepted stream stays blocking, as with a
                    // blocking listener: the hello answers once the
                    // command arrives, never with a `WouldBlock`.
                    stream.set_nonblocking(false).unwrap();
                    let mut read = BufReader::new(stream.try_clone().unwrap());
                    let hello = json!({
                        "kind": "hub_hello", "ts": 1,
                        "schema_version": contract::SCHEMA_VERSION, "payload": {},
                    });
                    writeln!(stream.try_clone().unwrap(), "{hello}").unwrap();
                    let mut line = String::new();
                    read.read_line(&mut line).unwrap();
                    events.lock().unwrap().push(format!("hub:{line}"));
                    let got: Value = serde_json::from_str(line.trim()).unwrap();
                    let id = got.get("id").and_then(Value::as_str).unwrap_or("?");
                    let answer = json!({
                        "kind": "command_accepted", "ts": 2,
                        "schema_version": contract::SCHEMA_VERSION,
                        "payload": {"command_id": id},
                    });
                    writeln!(stream.try_clone().unwrap(), "{answer}").unwrap();
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::yield_now();
                }
                Err(_) => return,
            }
        }
    });
    (handle, stop_tx)
}

#[test]
fn install_with_an_answering_hub_sends_stop_before_any_manager_call() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    let events: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (_hub, _shutdown) = stopping_hub(&setup.home(), Arc::clone(&events));
    let stub_events = Arc::clone(&events);
    let stub = Stub::new(move |line| {
        stub_events.lock().unwrap().push(format!("manager:{line}"));
        Ok(if line.contains(" print ") { no() } else { ok() })
    });
    let (result, out) = setup.install(&service, None, &stub, true);
    result.unwrap();
    assert_eq!(out, installed_line(&service));
    let events = events.lock().unwrap();
    // The hub logs the stop before it answers, and the install waits for
    // the answer before the manager runs: the stop is first.
    let first = events.first().expect("the stop is sent");
    assert!(
        first.starts_with("hub:"),
        "the stop comes before any manager call: {events:?}"
    );
    assert!(
        first.contains("\"command\":\"stop\""),
        "the command is stop: {first}"
    );
    assert!(
        first.contains("\"args\":{}"),
        "the stop carries no arguments: {first}"
    );
    assert!(
        events
            .iter()
            .skip(1)
            .all(|event| event.starts_with("manager:")),
        "only the stop reaches the hub: {events:?}"
    );
    assert!(events.len() > 1, "the manager still runs: {events:?}");
}

#[test]
fn a_second_identical_launchd_install_with_an_answering_hub_sends_no_stop() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    setup
        .install(&service, None, &Stub::unloaded(), false)
        .0
        .unwrap();
    let events: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (_hub, _shutdown) = stopping_hub(&setup.home(), Arc::clone(&events));
    let stub_events = Arc::clone(&events);
    let stub = Stub::new(move |line| {
        stub_events.lock().unwrap().push(format!("manager:{line}"));
        Ok(ok())
    });
    let (result, out) = setup.install(&service, None, &stub, true);
    result.unwrap();
    assert_eq!(out, installed_line(&service));
    let events = events.lock().unwrap();
    assert!(
        events.iter().all(|event| event.starts_with("manager:")),
        "an unchanged loaded install sends no stop: {events:?}"
    );
    assert_eq!(stub.calls(), [print(&service)]);
}

#[test]
fn a_second_identical_systemd_install_with_an_answering_hub_sends_no_stop() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    setup.install(&service, None, &Stub::ok(), false).0.unwrap();
    let events: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (_hub, _shutdown) = stopping_hub(&setup.home(), Arc::clone(&events));
    let stub = Stub::ok();
    let (result, out) = setup.install(&service, None, &stub, true);
    result.unwrap();
    assert_eq!(out, installed_line(&service));
    assert_eq!(
        stub.calls(),
        [systemctl(&format!("enable --now {}.service", service.name))]
    );
    assert!(
        events.lock().unwrap().is_empty(),
        "an unchanged loaded install sends no stop"
    );
}

#[test]
fn an_unchanged_unloaded_launchd_install_with_an_answering_hub_sends_stop() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, service.render(None).unwrap()).unwrap();
    let events: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (hub, shutdown) = stopping_hub(&setup.home(), Arc::clone(&events));
    let stub_events = Arc::clone(&events);
    let stub = Stub::new(move |line| {
        stub_events.lock().unwrap().push(format!("manager:{line}"));
        Ok(if line.contains(" print ") { no() } else { ok() })
    });
    let (result, out) = setup.install(&service, None, &stub, true);
    result.unwrap();
    assert_eq!(out, installed_line(&service));
    // The install returns only after the hub answers, and the answer
    // waits on the logged stop: the events below are complete without
    // joining. Shutting down ends the hub's loop, so the join reaps a
    // thread that already exited instead of waiting on a real clock.
    drop(shutdown);
    hub.join().unwrap();
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 3, "print, stop, bootstrap: {events:?}");
    assert_eq!(events[0], format!("manager:{}", print(&service)));
    assert!(
        events[1].starts_with("hub:") && events[1].contains("\"command\":\"stop\""),
        "the unchanged unloaded install still stops the hub: {events:?}"
    );
    assert_eq!(events[2], format!("manager:{}", bootstrap(&service)));
}

#[test]
fn install_without_an_answering_hub_connects_to_nothing() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::create_dir_all(setup.home().join("run")).unwrap();
    let listener = UnixListener::bind(setup.home().join("run/hub")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (result, out) = setup.install(&service, None, &Stub::unloaded(), false);
    result.unwrap();
    assert_eq!(out, installed_line(&service));
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == io::ErrorKind::WouldBlock),
        "install sends no stop without an answering hub"
    );
}

#[test]
fn install_without_a_port_removes_hub_port() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    setup
        .install(&service, Some(4040), &Stub::ok(), false)
        .0
        .unwrap();
    setup.install(&service, None, &Stub::ok(), false).0.unwrap();
    assert_eq!(setup.port(), None);
    assert!(
        fs::read_to_string(&service.unit)
            .unwrap()
            .starts_with("# hub.port none\n")
    );
}

#[test]
fn the_unit_file_is_mode_0644_and_no_temporary_file_remains() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    setup.install(&service, None, &Stub::ok(), false).0.unwrap();
    let mode = fs::metadata(&service.unit).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o644);
    let names: Vec<_> = fs::read_dir(setup.dir.path().join("units"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [service.unit.file_name().unwrap().to_owned()]);
}

fn uninstalled(service: &Service, stub: &Stub) -> (Result<(), CliError>, String) {
    let mut out = Vec::new();
    let result = uninstall(service, &stub.run, &mut out);
    (result, String::from_utf8(out).unwrap())
}

const REMOVED: &str = "Removed the hub's login service.\n";
const NOT_INSTALLED: &str = "The hub's login service is not installed.\n";

#[test]
fn launchd_uninstall_boots_out_a_loaded_service_then_deletes_the_plist() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, "x").unwrap();
    let stub = Stub::ok();
    let (result, out) = uninstalled(&service, &stub);
    result.unwrap();
    assert_eq!(stub.calls(), [print(&service), bootout(&service)]);
    assert!(!service.unit.exists());
    assert_eq!(out, REMOVED);
}

#[test]
fn launchd_uninstall_of_an_unloaded_service_deletes_the_plist() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, "x").unwrap();
    let stub = Stub::unloaded();
    let (result, out) = uninstalled(&service, &stub);
    result.unwrap();
    assert_eq!(stub.calls(), [print(&service)]);
    assert!(!service.unit.exists());
    assert_eq!(out, REMOVED);
}

#[test]
fn launchd_uninstall_of_a_loaded_service_with_no_plist_boots_it_out() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    let stub = Stub::ok();
    let (result, out) = uninstalled(&service, &stub);
    result.unwrap();
    assert_eq!(stub.calls(), [print(&service), bootout(&service)]);
    assert_eq!(out, REMOVED);
}

#[test]
fn launchd_uninstall_with_nothing_installed_says_so() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    let stub = Stub::unloaded();
    let (result, out) = uninstalled(&service, &stub);
    result.unwrap();
    assert_eq!(stub.calls(), [print(&service)]);
    assert_eq!(out, NOT_INSTALLED);
}

#[test]
fn a_failed_bootout_leaves_the_plist_for_a_retry() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, "x").unwrap();
    let stub = Stub::new(|line| {
        Ok(if line.contains(" bootout ") {
            boom()
        } else {
            ok()
        })
    });
    let (result, out) = uninstalled(&service, &stub);
    assert!(result.unwrap_err().to_string().contains("bootout"));
    assert!(service.unit.exists());
    assert_eq!(out, "");
}

#[test]
fn systemd_uninstall_disables_deletes_then_reloads() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    fs::write(&service.unit, "x").unwrap();
    let stub = Stub::ok();
    let (result, out) = uninstalled(&service, &stub);
    result.unwrap();
    assert_eq!(
        stub.calls(),
        [
            systemctl(&format!("disable --now {}.service", service.name)),
            systemctl("daemon-reload"),
        ]
    );
    assert!(!service.unit.exists());
    assert_eq!(out, REMOVED);
}

#[test]
fn systemd_uninstall_with_no_unit_runs_nothing() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    let stub = Stub::ok();
    let (result, out) = uninstalled(&service, &stub);
    result.unwrap();
    assert!(stub.calls().is_empty());
    assert_eq!(out, NOT_INSTALLED);
}

#[test]
fn a_failed_disable_leaves_the_unit_for_a_retry() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    fs::write(&service.unit, "x").unwrap();
    let stub = Stub::new(|_| Ok(boom()));
    assert!(uninstalled(&service, &stub).0.is_err());
    assert!(service.unit.exists());
}

#[test]
fn restart_kicks_an_installed_service_through_its_manager() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, "x").unwrap();
    let stub = Stub::ok();
    assert!(restart(&service, &stub.run).unwrap());
    assert_eq!(
        stub.calls(),
        [format!("launchctl kickstart -k gui/{UID}/{}", service.name)]
    );

    let service = setup.service(Manager::Systemd);
    fs::write(&service.unit, "x").unwrap();
    let stub = Stub::ok();
    assert!(restart(&service, &stub.run).unwrap());
    assert_eq!(
        stub.calls(),
        [systemctl(&format!("restart {}.service", service.name))]
    );
}

#[test]
fn restart_with_no_unit_file_runs_nothing() {
    for manager in [launchd(), Manager::Systemd] {
        let setup = Setup::new();
        let service = setup.service(manager);
        let stub = Stub::ok();
        assert!(!restart(&service, &stub.run).unwrap());
        assert!(stub.calls().is_empty());
    }
}

#[test]
fn a_failed_restart_is_an_error() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    fs::write(&service.unit, "x").unwrap();
    assert!(restart(&service, &Stub::new(|_| Ok(boom())).run).is_err());
}

/// A hub on `home`'s `run/hub` that speaks `hub_hello` to one client.
fn speaking_hub(home: &Path) -> std::thread::JoinHandle<()> {
    fs::create_dir_all(home.join("run")).unwrap();
    let listener = UnixListener::bind(home.join("run/hub")).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let hello = json!({
            "kind": "hub_hello", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION, "payload": {},
        });
        writeln!(stream, "{hello}").unwrap();
        // Hold the connection until the client closes it.
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap_or(0);
    })
}

#[test]
fn a_hub_answers_when_one_speaks_hub_hello() {
    let setup = Setup::new();
    let home = setup.home();
    let _hub = speaking_hub(&home);
    let clock = setup.clock.clone();
    assert!(fakes::within(
        "the probe",
        Deadline::after(DEADLINE),
        move || hub_answers(&home, &*clock, DEADLINE)
    ));
}

#[test]
fn no_hub_answers_on_an_absent_socket_and_none_is_started() {
    let setup = Setup::new();
    let home = setup.home();
    let clock = setup.clock.clone();
    let probed = home.clone();
    assert!(!fakes::within(
        "the probe",
        Deadline::after(DEADLINE),
        move || hub_answers(&probed, &*clock, DEADLINE)
    ));
    assert!(!home.join("run/hub").exists());
}

#[test]
fn system_run_reports_the_exit_code_and_stderr() {
    let dir = fakes::TempDir::new("cli-hub-runner");
    let marker = dir.path().join("runner").display().to_string();
    let watchdog = fakes::Watchdog::matching(&marker);
    let failing = marker.clone();
    let ran = fakes::within("sh", Deadline::after(DEADLINE), move || {
        system_run(
            "sh",
            &["-c".to_owned(), "echo oops >&2; exit 3".to_owned(), failing],
        )
    })
    .unwrap();
    assert_eq!(
        ran,
        Ran {
            success: false,
            code: Some(3),
            stderr: "oops\n".to_owned(),
        }
    );
    let ran = fakes::within("true", Deadline::after(DEADLINE), move || {
        system_run("sh", &["-c".to_owned(), "true".to_owned(), marker])
    })
    .unwrap();
    assert!(ran.success);
    assert_eq!(ran.code, Some(0));
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
}

#[test]
fn system_run_reports_a_missing_program_as_not_found() {
    let error = system_run("/nonexistent/fiber-launchctl", &[]).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

#[test]
fn read_unit_is_none_when_absent_some_when_present_and_an_error_otherwise() {
    let dir = fakes::TempDir::new("cli-hub-read-unit");
    let absent = dir.path().join("absent.service");
    assert_eq!(read_unit(&absent).unwrap(), None);
    let file = dir.path().join("hub.service");
    fs::write(&file, b"unit").unwrap();
    assert_eq!(read_unit(&file).unwrap(), Some(b"unit".to_vec()));
    // Reading a directory fails with `IsADirectory`, not `NotFound`.
    let error = read_unit(dir.path()).unwrap_err();
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert!(
        matches!(&error, CliError::UnitReadFailed { unit, .. } if unit == dir.path()),
        "{error}"
    );
}

#[test]
fn hub_restart_with_no_unit_file_restarts_nothing() {
    let dir = fakes::TempDir::new("cli-hub-restart");
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    assert!(!hub_restart(&home).unwrap());
}

#[test]
fn hub_restart_with_too_long_a_home_is_a_usage_error() {
    let home = PathBuf::from(format!("/tmp/{}", "h".repeat(300)));
    let error = hub_restart(&home).unwrap_err();
    assert_eq!(error.code, ErrorCode::Usage);
}

/// The child's marker: set, the test runs one exit-code wrapper and exits
/// with its code, without touching the service manager: every case fails
/// before any manager call, or runs no call.
const CHILD: &str = "FIBER_CLI_HUB_SERVICE_CHILD";

/// Which wrapper the child runs.
const CASE: &str = "FIBER_CLI_HUB_SERVICE_CASE";

/// How long the child may run before the test kills it and fails.
const CHILD_DEADLINE: Duration = Duration::from_secs(10);

/// Runs the child case `case` with `vars` in its environment and returns
/// its exit code.
#[track_caller]
fn child_exit(test: &str, case: &str, vars: &[(&str, &Path)]) -> i32 {
    let output = fakes::rerun_prepared(
        &format!("{test}::hub_wrappers_exit_without_the_manager"),
        &[(CHILD, "1"), (CASE, case)],
        Deadline::after(CHILD_DEADLINE),
        |command| {
            command.env_remove("XDG_CONFIG_HOME");
            for (key, value) in vars {
                command.env(key, value);
            }
        },
    );
    output.status.code().expect("the child exits with a code")
}

#[test]
fn hub_wrappers_exit_without_the_manager() {
    if std::env::var_os(CHILD).is_some() {
        let clock = FakeClock::new();
        let code = match std::env::var(CASE).unwrap().as_str() {
            "install-relative" => hub_install(None, &*clock),
            "install-unwritable" => hub_install(None, &*clock),
            "uninstall-fresh" => hub_uninstall(),
            "uninstall-relative" => hub_uninstall(),
            case => panic!("unknown hub wrapper child case: {case}"),
        };
        std::process::exit(code);
    }
    let name = module_path!().split_once("::").unwrap().1;
    // A relative `FIBER_HOME` is a usage error before anything runs.
    assert_eq!(
        child_exit(
            name,
            "install-relative",
            &[("FIBER_HOME", Path::new("relative"))]
        ),
        2
    );
    assert_eq!(
        child_exit(
            name,
            "uninstall-relative",
            &[("FIBER_HOME", Path::new("relative"))]
        ),
        2
    );
    let dir = fakes::TempDir::new("cli-hub-wrappers");
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    // A fresh home uninstalls nothing and exits 0 with no manager call.
    let user = dir.path().join("user");
    fs::create_dir_all(&user).unwrap();
    assert_eq!(
        child_exit(
            name,
            "uninstall-fresh",
            &[("FIBER_HOME", home.as_path()), ("HOME", user.as_path())]
        ),
        0
    );
    // A unit path under a file fails before any manager call.
    let file = dir.path().join("user-file");
    fs::write(&file, b"x").unwrap();
    assert_eq!(
        child_exit(
            name,
            "install-unwritable",
            &[("FIBER_HOME", home.as_path()), ("HOME", file.as_path())]
        ),
        1
    );
}

fn is_active(service: &Service) -> String {
    systemctl(&format!("is-active {}.service", service.name))
}

fn stop(service: &Service) -> String {
    systemctl(&format!("stop {}.service", service.name))
}

#[test]
fn stop_unloads_a_loaded_launchd_service_and_waits_it_out() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, service.render(None).unwrap()).unwrap();
    // Loaded before; after bootout launchd still knows it for one ask.
    let mut prints = 0;
    let stub = Stub::new(move |line| {
        if line.contains(" print ") {
            prints += 1;
            return Ok(if prints <= 2 { ok() } else { no() });
        }
        Ok(ok())
    });
    let stopped = stop_login_service(&service, &stub.run, &*setup.clock).unwrap();
    assert!(stopped);
    assert_eq!(
        stub.calls(),
        [
            print(&service),
            bootout(&service),
            print(&service),
            print(&service)
        ]
    );
    assert_eq!(
        setup.clock.now().duration_since(setup.clock.origin()),
        Duration::from_millis(100),
        "one poll interval between the last loaded ask and the gone one"
    );
    // The unit file stays: only the manager job goes.
    assert!(service.unit.exists());
}

#[test]
fn stop_skips_a_launchd_service_that_was_never_loaded() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, service.render(None).unwrap()).unwrap();
    let stub = Stub::unloaded();
    let stopped = stop_login_service(&service, &stub.run, &*setup.clock).unwrap();
    assert!(!stopped);
    assert_eq!(stub.calls(), [print(&service)]);
}

#[test]
fn stop_stops_an_active_systemd_service_and_keeps_the_file() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    let unit = service.render(None).unwrap();
    fs::write(&service.unit, &unit).unwrap();
    let stub = Stub::ok();
    let stopped = stop_login_service(&service, &stub.run, &*setup.clock).unwrap();
    assert!(stopped);
    assert_eq!(stub.calls(), [is_active(&service), stop(&service)]);
    assert_eq!(fs::read_to_string(&service.unit).unwrap(), unit);
}

#[test]
fn stop_skips_an_inactive_systemd_service_and_keeps_the_file() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    let unit = service.render(None).unwrap();
    fs::write(&service.unit, &unit).unwrap();
    let stub = Stub::new(|line| {
        Ok(if line.contains("is-active") {
            no()
        } else {
            ok()
        })
    });
    let stopped = stop_login_service(&service, &stub.run, &*setup.clock).unwrap();
    assert!(!stopped);
    assert_eq!(stub.calls(), [is_active(&service)]);
    assert_eq!(fs::read_to_string(&service.unit).unwrap(), unit);
}

#[test]
fn stop_without_a_unit_file_calls_nothing() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    let stub = Stub::ok();
    let stopped = stop_login_service(&service, &stub.run, &*setup.clock).unwrap();
    assert!(!stopped);
    assert!(stub.calls().is_empty());
}

#[test]
fn a_prompt_command_answers_through_the_bound() {
    let clock = FakeClock::new();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = Arc::clone(&clock);
    // The wall deadline fails a hang; it never decides: a frozen clock
    // leaves a wedged child parked, so the call runs on a worker while
    // this thread receives under the deadline.
    thread::spawn(move || {
        done_tx
            .send(system_run_bounded("true", &[], &*worker))
            .unwrap_or(());
    });
    let ran = Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("a prompt command answers")
        .unwrap();
    assert_eq!(
        ran,
        Ran {
            success: true,
            code: Some(0),
            stderr: String::new(),
        }
    );
    assert_eq!(clock.now(), clock.origin(), "no wait ran at all");
}

#[test]
fn a_slow_but_prompt_command_answers_without_a_kill() {
    let dir = fakes::TempDir::new("cli-manager-bound");
    let ready = fakes::children::Ready::new(dir.path());
    let block = dir.path().join("block.fifo");
    fakes::fifo(&block);
    // The child writes its pid, then blocks reading its fifo until the
    // test releases it: no real-clock sleep holds it.
    let script = format!(
        "echo $$ > '{}'; read -r _ < '{}'",
        ready.path().display(),
        block.display()
    );
    let clock = FakeClock::new();
    let (done_tx, done_rx) = mpsc::channel();
    let worker = Arc::clone(&clock);
    thread::spawn(move || {
        done_tx
            .send(system_run_bounded(
                "sh",
                &["-c".to_owned(), script],
                &*worker,
            ))
            .unwrap_or(());
    });
    let pids = ready.wait(Deadline::after(DEADLINE));
    assert_eq!(pids.len(), 1, "the child writes its own pid once");
    // The watchdog parks on its bound while the child holds the fifo:
    // the pause point, not a silent wait, proves the child is held.
    let due = clock.origin() + MANAGER_WAIT;
    assert!(
        clock.await_parked(due, Deadline::after(DEADLINE)),
        "a held child parks its watchdog"
    );
    // Opening a fifo for writing blocks until a reader arrives: the child
    // opens its read end only after writing its pid, so the release opens
    // on a thread and the test bounds it. A child that never opens fails
    // the release here instead of hanging the test past its wait.
    let (release_tx, release_rx) = mpsc::channel();
    let release_block = block.clone();
    thread::spawn(move || {
        release_tx
            .send(fs::write(&release_block, b"\n"))
            .unwrap_or(());
    });
    Deadline::after(DEADLINE)
        .recv(&release_rx)
        .expect("the release write completes")
        .unwrap();
    let ran = Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("the released child answers")
        .unwrap();
    assert!(ran.success);
    assert_eq!(ran.code, Some(0));
    assert_eq!(clock.now(), clock.origin(), "the fake deadline never moved");
}

#[test]
fn a_hung_command_is_killed_and_reaped_past_its_wait() {
    let dir = fakes::TempDir::new("cli-manager-bound");
    let ready = fakes::children::Ready::new(dir.path());
    let block = dir.path().join("block.fifo");
    fakes::fifo(&block);
    // The child blocks reading its own fifo forever: no pipe the runner
    // holds ever frees it. Its pid line names its process group, which
    // the runner leads.
    let script = format!(
        "echo $$ > '{}'; read -r _ < '{}'",
        ready.path().display(),
        block.display()
    );
    let clock = FakeClock::new();
    let due = clock.origin() + MANAGER_WAIT;
    let (done_tx, done_rx) = mpsc::channel();
    let worker = Arc::clone(&clock);
    thread::spawn(move || {
        done_tx
            .send(system_run_bounded(
                "sh",
                &["-c".to_owned(), script],
                &*worker,
            ))
            .unwrap_or(());
    });
    let pids = ready.wait(Deadline::after(DEADLINE));
    assert_eq!(pids.len(), 1, "the child writes its own pid once");
    let group = pids[0];
    // Past the wait, never at it: `>` and `==` mutants hang here instead
    // of killing.
    assert!(
        clock.await_parked(due, Deadline::after(DEADLINE)),
        "the watchdog parks at its wait"
    );
    clock.advance(MANAGER_WAIT + Duration::from_secs(1));
    let error = Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("the bound ends the run")
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        fakes::group_empties(group, Deadline::after(DEADLINE)),
        "the kill leaves no descendant behind"
    );
}

#[test]
fn restart_takeover_still_sends_the_bare_stop() {
    let dir = fakes::TempDir::new("cli-restart-bare-stop");
    let run = dir.path().join("run");
    fs::create_dir_all(&run).unwrap();
    let listener = UnixListener::bind(run.join("hub")).unwrap();
    let (line_tx, line_rx) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut hello = serde_json::to_vec(&json!({"kind": "hub_hello", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION, "payload": {}}))
        .unwrap();
        hello.push(b'\n');
        (&stream).write_all(&hello).unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        line_tx.send(line).unwrap_or(());
        let mut accepted = serde_json::to_vec(&json!({"kind": "command_accepted", "ts": 1,
            "schema_version": contract::SCHEMA_VERSION,
            "payload": {"command_id": "c_stop", "result": null}}))
        .unwrap();
        accepted.push(b'\n');
        (&stream).write_all(&accepted).unwrap();
    });
    let clock = FakeClock::new();
    super::stop_running_hub(dir.path(), &*clock);
    let line = Deadline::after(DEADLINE)
        .recv(&line_rx)
        .expect("the hub reads the stop");
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap(),
        json!({"id": "c_stop", "command": "stop", "args": {}}),
        "the restart takeover sends the bare stop with no sessions"
    );
}

#[test]
fn an_exited_child_with_a_lingering_descendant_is_reaped_within_its_wait() {
    let dir = fakes::TempDir::new("cli-manager-lingering");
    let ready = fakes::children::Ready::new(dir.path());
    // The direct child writes its pid, writes one stderr line, then exits
    // at once, leaving a background sleep holding the pipe open past the
    // bound: without the drain's own deadline the join would never return.
    let script = format!(
        "echo $$ > '{}'; echo lingering >&2; (sleep 60 &)",
        ready.path().display()
    );
    let clock = FakeClock::new();
    let (done_tx, done_rx) = mpsc::channel();
    let (wait_tx, wait_rx) = mpsc::channel();
    let worker = Arc::clone(&clock);
    thread::spawn(move || {
        // The hook fires on this same thread, inside the drain wait:
        // arming it here reaches the runner.
        super::on_drain_wait(move || {
            wait_tx.send(()).unwrap_or(());
        });
        done_tx
            .send(system_run_bounded(
                "sh",
                &["-c".to_owned(), script],
                &*worker,
            ))
            .unwrap_or(());
    });
    let pids = ready.wait(Deadline::after(DEADLINE));
    assert_eq!(pids.len(), 1, "the child writes its own pid once");
    let group = pids[0];
    // The direct child is already gone and the drain holds the pipe:
    // the hook fires only in the post-exit drain wait, never on a
    // live-child park at the same deadline, so advancing now takes
    // the expiry path instead of the timeout.
    Deadline::after(DEADLINE)
        .recv(&wait_rx)
        .expect("the runner reaches its drain wait");
    clock.advance(MANAGER_WAIT + Duration::from_secs(1));
    let ran = Deadline::after(DEADLINE)
        .recv(&done_rx)
        .expect("the bound ends the run")
        .unwrap();
    assert!(ran.success);
    assert_eq!(ran.code, Some(0));
    assert_eq!(ran.stderr, "lingering\n");
    assert!(
        fakes::group_empties(group, Deadline::after(DEADLINE)),
        "the kill leaves no descendant behind"
    );
}
