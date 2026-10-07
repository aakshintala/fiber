//! Install, uninstall and restart against a recording fake service
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
use std::sync::Arc;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::shapes::Failure;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::{RERUN, Ran, Runner, SystemRunner, hub_answers, install, restart, uninstall};
use crate::hub_unit::{Manager, Service, name};

/// One named deadline per blocking call.
const DEADLINE: Duration = Duration::from_secs(10);

const UID: u32 = 501;

type Script = Box<dyn FnMut(&str) -> io::Result<Ran>>;

/// A service manager that records each command line and answers it from a
/// script.
struct Fake {
    calls: RefCell<Vec<String>>,
    script: RefCell<Script>,
}

impl Fake {
    fn new(script: impl FnMut(&str) -> io::Result<Ran> + 'static) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            script: RefCell::new(Box::new(script)),
        }
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

impl Runner for Fake {
    fn run(&self, program: &str, args: &[String]) -> io::Result<Ran> {
        let line = format!("{program} {}", args.join(" "));
        self.calls.borrow_mut().push(line.clone());
        (self.script.borrow_mut())(&line)
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
        fake: &Fake,
        answers: bool,
    ) -> (Result<(), Failure>, String) {
        let mut out = Vec::new();
        let result = install(
            service,
            &self.home(),
            port,
            fake,
            &*self.clock,
            answers,
            &mut out,
            &super::write_unit,
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

const TAKEOVER: &str = "A hub a client started is running; the installed hub takes over once it \
                        exits, when no client has been connected for hub.idle_exit_ms.\n";

#[test]
fn launchd_not_loaded_writes_the_plist_and_bootstraps() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    let fake = Fake::unloaded();
    let (result, out) = setup.install(&service, Some(4040), &fake, false);
    result.unwrap();
    assert_eq!(fake.calls(), [print(&service), bootstrap(&service)]);
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
    let fake = Fake::unloaded();
    setup.install(&service, None, &fake, false).0.unwrap();
    assert_eq!(fake.calls(), [print(&service), bootstrap(&service)]);
}

#[test]
fn a_second_identical_launchd_install_only_asks() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    setup
        .install(&service, None, &Fake::unloaded(), false)
        .0
        .unwrap();
    let before = fs::read(&service.unit).unwrap();
    let fake = Fake::ok();
    let (result, out) = setup.install(&service, None, &fake, false);
    result.unwrap();
    assert_eq!(fake.calls(), [print(&service)]);
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
    let fake = Fake::new(move |line| {
        if line.contains(" print ") {
            prints += 1;
            return Ok(if prints <= 3 { ok() } else { no() });
        }
        Ok(ok())
    });
    setup.install(&service, Some(4040), &fake, false).0.unwrap();
    assert_eq!(
        fake.calls(),
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
    let fake = Fake::ok();
    let (result, out) = setup.install(&service, Some(4040), &fake, false);
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert!(error.message.ends_with(RERUN), "{}", error.message);
    // Asked at 0, 100 ms, ..., 10 s after the bootout.
    let calls = fake.calls();
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
    let fake = Fake::new(move |line| {
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
        .install(&service, Some(4040), &fake, false)
        .0
        .unwrap_err();
    assert_eq!(
        error.message,
        format!(
            "`launchctl bootstrap gui/{UID} {}` failed with exit code 5: boom. {RERUN}",
            service.unit.display()
        )
    );
    assert_eq!(fs::read_to_string(&service.unit).unwrap(), previous);

    let rerun = Fake::unloaded();
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
    setup.install(&service, None, &Fake::ok(), false).0.unwrap();
    let previous = fs::read(&service.unit).unwrap();
    let rendered = service.render(Some(4040)).unwrap();
    assert_ne!(previous, rendered.as_bytes());

    // The write leaves the new bytes in place (the rename succeeded)
    // then fails (its directory sync failed), like `write_atomic` after
    // the rename.
    let failing = |path: &Path, bytes: &[u8]| -> Result<(), Failure> {
        fs::write(path, bytes).unwrap();
        Err(crate::failed(
            ErrorCode::IoFailed,
            format!("syncing {}: directory sync failed", path.display()),
        ))
    };
    let mut out = Vec::new();
    let error = install(
        &service,
        &setup.home(),
        Some(4040),
        &Fake::ok(),
        &*setup.clock,
        false,
        &mut out,
        &failing,
    )
    .unwrap_err();
    assert!(error.message.ends_with(RERUN), "{}", error.message);
    assert!(
        error.message.contains("directory sync failed"),
        "{}",
        error.message
    );
    assert_eq!(fs::read(&service.unit).unwrap(), previous);
    assert!(out.is_empty(), "{out:?}");

    let rerun = Fake::ok();
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
    let fake = Fake::ok();
    let (result, out) = setup.install(&service, None, &fake, false);
    result.unwrap();
    assert_eq!(fake.calls(), changed_row(&service));
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
        .install(&service, Some(4040), &Fake::ok(), false)
        .0
        .unwrap();
    let fake = Fake::ok();
    setup.install(&service, Some(4040), &fake, false).0.unwrap();
    assert_eq!(
        fake.calls(),
        [systemctl(&format!("enable --now {}.service", service.name))]
    );
}

#[test]
fn a_port_only_change_whose_restart_fails_is_restarted_by_the_rerun() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    setup.install(&service, None, &Fake::ok(), false).0.unwrap();
    let previous = fs::read(&service.unit).unwrap();

    let fake = Fake::new(|line| {
        Ok(if line.contains(" restart ") {
            boom()
        } else {
            ok()
        })
    });
    let error = setup
        .install(&service, Some(4040), &fake, false)
        .0
        .unwrap_err();
    assert_eq!(
        error.message,
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

    let rerun = Fake::ok();
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
    let fake = Fake::new(|line| {
        Ok(if line.contains("daemon-reload") {
            boom()
        } else {
            ok()
        })
    });
    let error = setup.install(&service, None, &fake, false).0.unwrap_err();
    assert!(error.message.contains("daemon-reload"), "{}", error.message);
    assert!(!service.unit.exists());
    assert_eq!(fake.calls(), [systemctl("daemon-reload")]);

    let rerun = Fake::ok();
    setup.install(&service, None, &rerun, false).0.unwrap();
    assert_eq!(rerun.calls(), changed_row(&service));
}

#[test]
fn an_unchanged_unit_is_left_as_it_is_when_its_call_fails() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    setup.install(&service, None, &Fake::ok(), false).0.unwrap();
    let before = fs::read(&service.unit).unwrap();
    let fake = Fake::new(|_| Ok(boom()));
    let error = setup.install(&service, None, &fake, false).0.unwrap_err();
    assert!(error.message.contains("enable --now"), "{}", error.message);
    assert_eq!(fs::read(&service.unit).unwrap(), before);
}

#[test]
fn a_restore_that_fails_names_the_unit_file_and_says_to_uninstall() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    let units = setup.dir.path().join("units");
    let fake = Fake::new(move |_| {
        // The unit is written; the directory then refuses the restore's
        // removal.
        fs::set_permissions(&units, fs::Permissions::from_mode(0o555)).unwrap();
        Ok(boom())
    });
    let error = setup.install(&service, None, &fake, false).0.unwrap_err();
    fs::set_permissions(
        setup.dir.path().join("units"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(
        error.message.contains(&format!(
            "The unit file {} could not be put back",
            service.unit.display()
        )),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("run fiber hub uninstall"),
        "{}",
        error.message
    );
    assert!(!error.message.contains(RERUN), "{}", error.message);
}

#[test]
fn a_missing_service_manager_says_what_the_installed_hub_needs() {
    for (manager, sentence) in [
        (
            launchd(),
            "`launchctl` was not found; an installed hub needs launchd.",
        ),
        (
            Manager::Systemd,
            "`systemctl` was not found; an installed hub needs systemd's user instance.",
        ),
    ] {
        let setup = Setup::new();
        let service = setup.service(manager);
        let fake = Fake::new(|_| Err(io::Error::new(io::ErrorKind::NotFound, "no such file")));
        let error = setup.install(&service, None, &fake, false).0.unwrap_err();
        assert_eq!(error.code, ErrorCode::IoFailed);
        assert!(error.message.starts_with(sentence), "{}", error.message);
        assert!(!service.unit.exists());
    }
}

#[test]
fn another_spawn_failure_names_the_program() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    let fake = Fake::new(|_| Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")));
    let error = setup.install(&service, None, &fake, false).0.unwrap_err();
    assert!(
        error.message.starts_with("running `systemctl`: denied"),
        "{}",
        error.message
    );
}

#[test]
fn a_hub_answering_while_the_service_is_not_loaded_prints_the_takeover_line() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    let (result, out) = setup.install(&service, None, &Fake::unloaded(), true);
    result.unwrap();
    assert_eq!(out, format!("{}{TAKEOVER}", installed_line(&service)));

    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    let (result, out) = setup.install(&service, None, &Fake::ok(), true);
    result.unwrap();
    assert_eq!(out, format!("{}{TAKEOVER}", installed_line(&service)));
}

#[test]
fn a_hub_answering_while_the_service_is_loaded_prints_no_takeover_line() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, service.render(None).unwrap()).unwrap();
    let (result, out) = setup.install(&service, None, &Fake::ok(), true);
    result.unwrap();
    assert_eq!(out, installed_line(&service));

    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    setup.install(&service, None, &Fake::ok(), false).0.unwrap();
    let (result, out) = setup.install(&service, None, &Fake::ok(), true);
    result.unwrap();
    assert_eq!(out, installed_line(&service));
}

#[test]
fn install_without_a_port_removes_hub_port() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    setup
        .install(&service, Some(4040), &Fake::ok(), false)
        .0
        .unwrap();
    setup.install(&service, None, &Fake::ok(), false).0.unwrap();
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
    setup.install(&service, None, &Fake::ok(), false).0.unwrap();
    let mode = fs::metadata(&service.unit).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o644);
    let names: Vec<_> = fs::read_dir(setup.dir.path().join("units"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [service.unit.file_name().unwrap().to_owned()]);
}

fn uninstalled(service: &Service, fake: &Fake) -> (Result<(), Failure>, String) {
    let mut out = Vec::new();
    let result = uninstall(service, fake, &mut out);
    (result, String::from_utf8(out).unwrap())
}

const REMOVED: &str = "Removed the hub's login service.\n";
const NOT_INSTALLED: &str = "The hub's login service is not installed.\n";

#[test]
fn launchd_uninstall_boots_out_a_loaded_service_then_deletes_the_plist() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, "x").unwrap();
    let fake = Fake::ok();
    let (result, out) = uninstalled(&service, &fake);
    result.unwrap();
    assert_eq!(fake.calls(), [print(&service), bootout(&service)]);
    assert!(!service.unit.exists());
    assert_eq!(out, REMOVED);
}

#[test]
fn launchd_uninstall_of_an_unloaded_service_deletes_the_plist() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, "x").unwrap();
    let fake = Fake::unloaded();
    let (result, out) = uninstalled(&service, &fake);
    result.unwrap();
    assert_eq!(fake.calls(), [print(&service)]);
    assert!(!service.unit.exists());
    assert_eq!(out, REMOVED);
}

#[test]
fn launchd_uninstall_of_a_loaded_service_with_no_plist_boots_it_out() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    let fake = Fake::ok();
    let (result, out) = uninstalled(&service, &fake);
    result.unwrap();
    assert_eq!(fake.calls(), [print(&service), bootout(&service)]);
    assert_eq!(out, REMOVED);
}

#[test]
fn launchd_uninstall_with_nothing_installed_says_so() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    let fake = Fake::unloaded();
    let (result, out) = uninstalled(&service, &fake);
    result.unwrap();
    assert_eq!(fake.calls(), [print(&service)]);
    assert_eq!(out, NOT_INSTALLED);
}

#[test]
fn a_failed_bootout_leaves_the_plist_for_a_retry() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, "x").unwrap();
    let fake = Fake::new(|line| {
        Ok(if line.contains(" bootout ") {
            boom()
        } else {
            ok()
        })
    });
    let (result, out) = uninstalled(&service, &fake);
    assert!(result.unwrap_err().message.contains("bootout"));
    assert!(service.unit.exists());
    assert_eq!(out, "");
}

#[test]
fn systemd_uninstall_disables_deletes_then_reloads() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    fs::write(&service.unit, "x").unwrap();
    let fake = Fake::ok();
    let (result, out) = uninstalled(&service, &fake);
    result.unwrap();
    assert_eq!(
        fake.calls(),
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
    let fake = Fake::ok();
    let (result, out) = uninstalled(&service, &fake);
    result.unwrap();
    assert!(fake.calls().is_empty());
    assert_eq!(out, NOT_INSTALLED);
}

#[test]
fn a_failed_disable_leaves_the_unit_for_a_retry() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    fs::write(&service.unit, "x").unwrap();
    let fake = Fake::new(|_| Ok(boom()));
    assert!(uninstalled(&service, &fake).0.is_err());
    assert!(service.unit.exists());
}

#[test]
fn restart_kicks_an_installed_service_through_its_manager() {
    let setup = Setup::new();
    let service = setup.service(launchd());
    fs::write(&service.unit, "x").unwrap();
    let fake = Fake::ok();
    assert!(restart(&service, &fake).unwrap());
    assert_eq!(
        fake.calls(),
        [format!("launchctl kickstart -k gui/{UID}/{}", service.name)]
    );

    let service = setup.service(Manager::Systemd);
    fs::write(&service.unit, "x").unwrap();
    let fake = Fake::ok();
    assert!(restart(&service, &fake).unwrap());
    assert_eq!(
        fake.calls(),
        [systemctl(&format!("restart {}.service", service.name))]
    );
}

#[test]
fn restart_with_no_unit_file_runs_nothing() {
    for manager in [launchd(), Manager::Systemd] {
        let setup = Setup::new();
        let service = setup.service(manager);
        let fake = Fake::ok();
        assert!(!restart(&service, &fake).unwrap());
        assert!(fake.calls().is_empty());
    }
}

#[test]
fn a_failed_restart_is_an_error() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    fs::write(&service.unit, "x").unwrap();
    assert!(restart(&service, &Fake::new(|_| Ok(boom()))).is_err());
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
    assert!(fakes::within("the probe", DEADLINE, move || hub_answers(
        &home, &*clock, DEADLINE
    )));
}

#[test]
fn no_hub_answers_on_an_absent_socket_and_none_is_started() {
    let setup = Setup::new();
    let home = setup.home();
    let clock = setup.clock.clone();
    let probed = home.clone();
    assert!(!fakes::within("the probe", DEADLINE, move || hub_answers(
        &probed, &*clock, DEADLINE
    )));
    assert!(!home.join("run/hub").exists());
}

#[test]
fn the_system_runner_reports_the_exit_code_and_stderr() {
    let dir = fakes::TempDir::new("cli-hub-runner");
    let marker = dir.path().join("runner").display().to_string();
    let watchdog = fakes::Watchdog::matching(&marker);
    let failing = marker.clone();
    let ran = fakes::within("sh", DEADLINE, move || {
        SystemRunner.run(
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
    let ran = fakes::within("true", DEADLINE, move || {
        SystemRunner.run("sh", &["-c".to_owned(), "true".to_owned(), marker])
    })
    .unwrap();
    assert!(ran.success);
    assert_eq!(ran.code, Some(0));
    watchdog.stand_down(DEADLINE);
}

#[test]
fn the_system_runner_reports_a_missing_program_as_not_found() {
    let error = SystemRunner
        .run("/nonexistent/fiber-launchctl", &[])
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

#[test]
fn a_failure_with_no_stderr_or_exit_code_says_so() {
    let setup = Setup::new();
    let service = setup.service(Manager::Systemd);
    fs::write(&service.unit, "x").unwrap();
    let killed = Fake::new(|_| {
        Ok(Ran {
            success: false,
            code: None,
            stderr: "\n".to_owned(),
        })
    });
    assert_eq!(
        restart(&service, &killed).unwrap_err().message,
        format!(
            "`systemctl --user restart {}.service` failed with no exit code.",
            service.name
        )
    );
}
