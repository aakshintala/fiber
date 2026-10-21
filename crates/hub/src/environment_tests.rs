//! Tests for the hub's shared environment (`docs/invocation.md`, "A
//! session's environment"): the overlay, the blocking `get`, the
//! `environment_built` lines, and `close` against a running capture.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::events::VariablesSource;
use fakes::Deadline;

use super::capture::Failed;
use super::{Environment, Environments};

/// How long a wait that must succeed holds, on the wall clock.
const WITHIN: Duration = Duration::from_secs(10);

/// How long a wait that must see nothing holds: several of the capture's
/// 20 ms wall-clock polls. `get` and `close` wake on notification rather
/// than polling, so this only needs to exceed a thread round-trip: an
/// early return answers at once, while a correct wait holds until the test
/// releases it.
const QUIET: Duration = Duration::from_millis(300);

/// A home under a held temporary directory, with a diagnostic log.
struct Temp {
    /// Holds the directory.
    _held: fakes::TempDir,
    /// The directory.
    dir: PathBuf,
    /// The hub home the overlay points at.
    home: PathBuf,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("he");
        let dir = held.path().to_path_buf();
        let home = dir.join("home");
        fs::create_dir_all(&home).unwrap();
        Self {
            _held: held,
            dir,
            home,
        }
    }

    /// The diagnostic log writing to `home`.
    fn diag(&self, clock: Arc<fakes::clock::FakeClock>) -> log::diag::Diag {
        crate::diag::open(&self.home, clock)
    }

    /// The parsed lines of `home`'s `logs/hub.log`.
    fn log_lines(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.home.join("logs").join("hub.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

/// A fake `$SHELL` under `temp` running `body`, with the pid prologue.
fn fake_shell(temp: &Temp, body: &str) -> PathBuf {
    let nonce = match fakes::check_run() {
        Some((name, value)) => format!("export {name}={}\n", value.to_string_lossy()),
        None => String::new(),
    };
    let body = format!(
        "export FAKE_DIR={}\necho $$ > $FAKE_DIR/pid\n{}{}",
        temp.dir.display(),
        nonce,
        body
    );
    fakes::script(&temp.dir, "shell", &body)
}

/// The inherited environment handing `shell` as `$SHELL`.
fn inherited(shell: &Path, home: &Path) -> Vec<(OsString, OsString)> {
    vec![
        (OsString::from("HOME"), home.as_os_str().to_owned()),
        (OsString::from("USER"), OsString::from("fiber-test")),
        (OsString::from("SHELL"), shell.as_os_str().to_owned()),
    ]
}

/// The shell's pid, read under a wall-clock deadline, with its watchdog
/// armed: the shell leads its own session and group.
fn watchdog(dir: &Path) -> (u32, fakes::Watchdog) {
    let deadline = Deadline::after(WITHIN);
    loop {
        if let Ok(text) = fs::read_to_string(dir.join("pid"))
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            return (pid, fakes::Watchdog::group(pid));
        }
        if deadline.left().is_zero() {
            panic!("waited for the fake shell to report its pid");
        }
        thread::yield_now();
    }
}

/// The named variable's value, when one is there.
fn get(vars: &[(OsString, OsString)], name: &str) -> Option<String> {
    vars.iter()
        .find_map(|(n, v)| (n == name).then(|| v.to_string_lossy().into_owned()))
}

/// `first` on a thread, received under one deadline: a blocking capture
/// never parks the test thread past the wait.
fn first_wait(
    environments: &Arc<Environments>,
    clock: &Arc<fakes::clock::FakeClock>,
    diag: log::diag::Diag,
) {
    let environments = Arc::clone(environments);
    let clock = Arc::clone(clock);
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        environments.first(&*clock, &diag);
        done_tx.send(()).unwrap_or(());
    });
    Deadline::after(WITHIN)
        .recv(&done_rx)
        .expect("first returns");
}

/// `refresh` on a thread, received under one deadline.
fn refresh_wait(
    environments: &Arc<Environments>,
    clock: &Arc<fakes::clock::FakeClock>,
    diag: log::diag::Diag,
) -> Arc<Environment> {
    let environments = Arc::clone(environments);
    let clock = Arc::clone(clock);
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done_tx
            .send(environments.refresh(&*clock, &diag))
            .unwrap_or(());
    });
    Deadline::after(WITHIN)
        .recv(&done_rx)
        .expect("refresh returns")
}

#[test]
fn a_capture_replaces_a_captured_fiber_home_with_the_hub_home() {
    let temp = Temp::new();
    let shell = fake_shell(
        &temp,
        "export FIBER_HOME=/elsewhere\nexec /bin/sh -c \"$4\"",
    );
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(inherited(&shell, &temp.dir), &temp.home);
    environments.first(&*clock, &temp.diag(Arc::clone(&clock)));
    let environment = environments.get();
    assert_eq!(environment.source, VariablesSource::LoginShell);
    let homes: Vec<String> = environment
        .vars
        .iter()
        .filter(|(name, _)| name == "FIBER_HOME")
        .map(|(_, value)| value.to_string_lossy().into_owned())
        .collect();
    assert_eq!(homes.len(), 1, "exactly one FIBER_HOME");
    assert_eq!(homes[0], temp.home.to_string_lossy());
    let lines = temp.log_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["level"], "info");
    assert_eq!(lines[0]["code"], "environment_built");
    assert_eq!(
        lines[0]["message"],
        "Sessions start with the login shell's environment."
    );
}

#[test]
fn a_fallback_replaces_an_inherited_fiber_home_with_the_hub_home() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let inherited = vec![
        (OsString::from("HOME"), temp.dir.as_os_str().to_owned()),
        (OsString::from("FIBER_HOME"), OsString::from("/other")),
    ];
    let environments = Environments::new(inherited, &temp.home);
    environments.first(&*clock, &temp.diag(Arc::clone(&clock)));
    let environment = environments.get();
    assert_eq!(environment.source, VariablesSource::Inherited);
    let homes: Vec<String> = environment
        .vars
        .iter()
        .filter(|(name, _)| name == "FIBER_HOME")
        .map(|(_, value)| value.to_string_lossy().into_owned())
        .collect();
    assert_eq!(homes, vec![temp.home.to_string_lossy().to_string()]);
}

#[test]
fn a_fallback_without_a_fiber_home_gains_the_hub_home() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let inherited = vec![(OsString::from("HOME"), temp.dir.as_os_str().to_owned())];
    let environments = Environments::new(inherited, &temp.home);
    environments.first(&*clock, &temp.diag(Arc::clone(&clock)));
    let environment = environments.get();
    assert_eq!(environment.source, VariablesSource::Inherited);
    assert_eq!(
        get(&environment.vars, "FIBER_HOME").as_deref(),
        Some(temp.home.to_string_lossy().as_ref())
    );
    let lines = temp.log_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["level"], "warn");
    assert_eq!(lines[0]["code"], "environment_built");
    assert_eq!(
        lines[0]["message"],
        "The login-shell capture failed because SHELL is not set, so sessions start with the hub's inherited environment."
    );
}

#[test]
fn get_blocks_until_the_first_capture_publishes() {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "export MARK=1\nexec /bin/sh -c \"$4\"");
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(inherited(&shell, &temp.dir), &temp.home);
    let waiting = Arc::clone(&environments);
    let (started_tx, started_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        started_tx.send(()).unwrap_or(());
        done_tx.send(waiting.get()).unwrap_or(());
    });
    // Signal that the `get` thread started: the bounded wait below then
    // proves it blocks, rather than passing because the thread never ran.
    Deadline::after(WITHIN)
        .recv(&started_rx)
        .expect("the get thread starts");
    assert!(
        Deadline::after(QUIET).recv(&done_rx).is_err(),
        "get blocks while nothing published"
    );
    environments.first(&*clock, &temp.diag(Arc::clone(&clock)));
    let environment = Deadline::after(WITHIN)
        .recv(&done_rx)
        .expect("get returns after the first publish");
    assert_eq!(get(&environment.vars, "MARK").as_deref(), Some("1"));
}

#[test]
fn close_with_nothing_running_returns_at_once_and_is_idempotent() {
    let environments = Environments::ready(Environment {
        vars: vec![(OsString::from("PATH"), OsString::from("/usr/bin"))],
        source: VariablesSource::Inherited,
    });
    environments.close();
    environments.close();
    assert_eq!(environments.get().source, VariablesSource::Inherited);
}

#[test]
fn close_during_a_hung_first_capture_returns_with_the_fallback() {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "exec sleep 600");
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(inherited(&shell, &temp.dir), &temp.home);
    let (pid, watchdog) = {
        let first = Arc::clone(&environments);
        let clock = Arc::clone(&clock);
        let diag = temp.diag(Arc::clone(&clock));
        thread::spawn(move || first.first(&*clock, &diag));
        watchdog(&temp.dir)
    };
    let waiting = Arc::clone(&environments);
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done_tx.send(waiting.get()).unwrap_or(());
    });
    environments.close();
    let environment = Deadline::after(WITHIN)
        .recv(&done_rx)
        .expect("a get blocked on the first capture returns");
    assert_eq!(environment.source, VariablesSource::Inherited);
    assert_eq!(
        get(&environment.vars, "FIBER_HOME").as_deref(),
        Some(temp.home.to_string_lossy().as_ref())
    );
    assert!(fakes::pids_exit(&[pid], WITHIN), "the shell is reaped");
    assert!(
        temp.log_lines()
            .iter()
            .all(|line| line["code"] != "environment_built"),
        "a cancelled capture writes no line"
    );
    watchdog.stand_down(WITHIN);
}

#[test]
fn value_reads_present_names_and_holds_absent_ones() {
    let environment = Environment {
        vars: vec![
            (OsString::from("PRESENT"), OsString::from("kept")),
            (OsString::from("PATH"), OsString::from("/usr/bin")),
        ],
        source: VariablesSource::Inherited,
    };
    assert_eq!(environment.value("PRESENT").as_deref(), Some("kept"));
    assert_eq!(environment.value("ABSENT"), None);
}

#[test]
fn ready_never_captures() {
    let temp = Temp::new();
    let _shell = fake_shell(&temp, "exec sleep 600");
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::ready(Environment {
        vars: vec![(OsString::from("PATH"), OsString::from("/usr/bin"))],
        source: VariablesSource::Inherited,
    });
    environments.first(&*clock, &temp.diag(Arc::clone(&clock)));
    assert!(!temp.dir.join("pid").exists(), "no shell spawned");
    assert!(temp.log_lines().is_empty(), "no line written");
}

#[test]
fn first_failed_publishes_the_fallback_with_its_line() {
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(Vec::new(), &temp.home);
    environments.first_failed(
        Failed::Spawn(std::io::ErrorKind::NotFound),
        &temp.diag(Arc::clone(&clock)),
    );
    let environment = environments.get();
    assert_eq!(environment.source, VariablesSource::Inherited);
    let lines = temp.log_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["level"], "warn");
    assert!(
        lines[0]["message"]
            .as_str()
            .unwrap()
            .contains("the shell could not start")
    );
}

#[test]
fn no_variable_value_reaches_the_log() {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "export FAKE_SECRET=s3cret\nexec /bin/sh -c \"$4\"");
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(inherited(&shell, &temp.dir), &temp.home);
    environments.first(&*clock, &temp.diag(Arc::clone(&clock)));
    let log = fs::read_to_string(temp.home.join("logs").join("hub.log")).unwrap_or_default();
    assert!(!log.contains("s3cret"), "no value in the log");
}

#[test]
fn a_capture_queued_on_the_lock_spawns_nothing_after_close() {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "exec sleep 600");
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(inherited(&shell, &temp.dir), &temp.home);
    // The first capture holds the capture lock on its hung shell.
    let first = Arc::clone(&environments);
    let first_clock = Arc::clone(&clock);
    let first_diag = temp.diag(Arc::clone(&clock));
    let first_done = thread::spawn(move || first.first(&*first_clock, &first_diag));
    let (pid, watchdog) = watchdog(&temp.dir);
    // The queued capture's spawn would rewrite the pid file: remove it,
    // so any spawn after `close` is seen.
    fs::remove_file(temp.dir.join("pid")).unwrap();
    let second = Arc::clone(&environments);
    let second_clock = Arc::clone(&clock);
    let second_diag = temp.diag(Arc::clone(&clock));
    let second_done = thread::spawn(move || second.first(&*second_clock, &second_diag));
    environments.close();
    first_done.join().expect("the first capture ends");
    second_done.join().expect("the queued capture ends");
    assert!(fakes::pids_exit(&[pid], WITHIN), "the shell is reaped");
    assert!(
        !temp.dir.join("pid").exists(),
        "the queued capture spawned nothing"
    );
    assert!(
        temp.log_lines()
            .iter()
            .all(|line| line["code"] != "environment_built"),
        "a cancelled capture writes no line"
    );
    watchdog.stand_down(WITHIN);
}

#[test]
fn first_publishes_the_fallback_synchronously() {
    // No `SHELL`: the capture fails without spawning, publishing the
    // fallback and writing its line before `first` returns, so the
    // assertions below fail at once when a mutant breaks the publish
    // instead of hanging in `get`.
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let inherited = vec![(OsString::from("HOME"), temp.dir.as_os_str().to_owned())];
    let environments = Environments::new(inherited, &temp.home);
    environments.first(&*clock, &temp.diag(Arc::clone(&clock)));
    let lines = temp.log_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["level"], "warn");
    assert_eq!(lines[0]["code"], "environment_built");
    assert_eq!(
        lines[0]["message"],
        "The login-shell capture failed because SHELL is not set, so sessions start with the hub's inherited environment."
    );
    let environment = environments.get();
    assert_eq!(environment.source, VariablesSource::Inherited);
    assert_eq!(
        get(&environment.vars, "FIBER_HOME").as_deref(),
        Some(temp.home.to_string_lossy().as_ref())
    );
}

#[test]
fn close_waits_for_a_running_capture() {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "export MARK=1\nexec /bin/sh -c \"$4\"");
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(inherited(&shell, &temp.dir), &temp.home);
    // Hold the capture between its end and its publish: the close below
    // cannot return until the hook is released.
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    *support::lock(&environments.before_publish) = Some(Box::new(move || {
        held_tx.send(()).unwrap_or(());
        Deadline::after(WITHIN).recv(&release_rx).unwrap_or(());
    }));
    let first = Arc::clone(&environments);
    let first_clock = Arc::clone(&clock);
    let first_diag = temp.diag(Arc::clone(&clock));
    let first_done = thread::spawn(move || first.first(&*first_clock, &first_diag));
    Deadline::after(WITHIN)
        .recv(&held_rx)
        .expect("the capture holds before publishing");
    let closing = Arc::clone(&environments);
    let (close_tx, close_rx) = mpsc::channel();
    thread::spawn(move || {
        closing.close();
        close_tx.send(()).unwrap_or(());
    });
    // Signal that `close` began: it set `closed` before waiting, so the
    // bounded wait below proves it waits for the held capture rather than
    // passing because its thread never ran.
    let began = Deadline::after(WITHIN);
    loop {
        if environments.is_closed() {
            break;
        }
        if began.left().is_zero() {
            panic!("waited for close to begin");
        }
        thread::yield_now();
    }
    assert!(
        Deadline::after(QUIET).recv(&close_rx).is_err(),
        "close waits for the running capture"
    );
    release_tx.send(()).unwrap_or(());
    Deadline::after(WITHIN)
        .recv(&close_rx)
        .expect("close returns once the capture ends");
    first_done.join().expect("the first capture ends");
    // The close published the fallback first, so the released capture
    // publishes silently.
    let environment = environments.get();
    assert_eq!(environment.source, VariablesSource::Inherited);
    assert!(
        temp.log_lines()
            .iter()
            .all(|line| line["code"] != "environment_built"),
        "a cancelled capture writes no line"
    );
}

#[test]
fn first_failed_after_a_completed_first_is_ignored() {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "export MARK=1\nexec /bin/sh -c \"$4\"");
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(inherited(&shell, &temp.dir), &temp.home);
    environments.first(&*clock, &temp.diag(Arc::clone(&clock)));
    let published = environments.get();
    assert_eq!(published.source, VariablesSource::LoginShell);
    assert_eq!(get(&published.vars, "MARK").as_deref(), Some("1"));
    // The completed capture ended its admission, so a late failure for the
    // admitted first capture publishes nothing and writes no line.
    environments.first_failed(
        Failed::Spawn(std::io::ErrorKind::NotFound),
        &temp.diag(Arc::clone(&clock)),
    );
    let kept = environments.get();
    assert_eq!(kept.source, VariablesSource::LoginShell);
    assert_eq!(get(&kept.vars, "MARK").as_deref(), Some("1"));
    let lines = temp.log_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["level"], "info");
    assert_eq!(lines[0]["code"], "environment_built");
}

#[test]
fn first_failed_publishes_the_fallback_synchronously() {
    // The publish happens before `first_failed` returns: a bounded `get`
    // proves it without hanging when a mutant breaks the publish.
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(Vec::new(), &temp.home);
    environments.first_failed(
        Failed::Spawn(std::io::ErrorKind::NotFound),
        &temp.diag(Arc::clone(&clock)),
    );
    let waiting = Arc::clone(&environments);
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        done_tx.send(waiting.get()).unwrap_or(());
    });
    let environment = Deadline::after(QUIET)
        .recv(&done_rx)
        .expect("first_failed publishes before it returns");
    assert_eq!(environment.source, VariablesSource::Inherited);
}

#[test]
fn close_before_first_returns_with_the_fallback() {
    // Forces the startup/shutdown interleaving: `close` runs before `first`
    // runs. Without ending the reserved admission on the early return,
    // `close` waits forever and the bounded wait below fails.
    let temp = Temp::new();
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(Vec::new(), &temp.home);
    let closing = Arc::clone(&environments);
    let (close_tx, close_rx) = mpsc::channel();
    thread::spawn(move || {
        closing.close();
        close_tx.send(()).unwrap_or(());
    });
    // Signal that `close` began (it published the fallback and waits for
    // the reserved admission): `first` below therefore runs after `close`.
    let began = Deadline::after(WITHIN);
    loop {
        if environments.is_closed() {
            break;
        }
        if began.left().is_zero() {
            panic!("waited for close to begin");
        }
        thread::yield_now();
    }
    environments.first(&*clock, &temp.diag(Arc::clone(&clock)));
    Deadline::after(WITHIN)
        .recv(&close_rx)
        .expect("close returns once first ends the admission");
    let environment = environments.get();
    assert_eq!(environment.source, VariablesSource::Inherited);
    assert_eq!(
        get(&environment.vars, "FIBER_HOME").as_deref(),
        Some(temp.home.to_string_lossy().as_ref())
    );
    assert!(
        temp.log_lines()
            .iter()
            .all(|line| line["code"] != "environment_built"),
        "a cancelled capture writes no line"
    );
}

#[test]
fn debug_holds_names_but_no_values() {
    let environment = Environment {
        vars: vec![
            (OsString::from("SECRET"), OsString::from("s3cret-value")),
            (OsString::from("PATH"), OsString::from("/secret/bin")),
        ],
        source: VariablesSource::LoginShell,
    };
    let shown = format!("{environment:?}");
    assert!(shown.contains("SECRET"), "names are shown: {shown}");
    assert!(shown.contains("PATH"), "names are shown: {shown}");
    assert!(!shown.contains("s3cret-value"), "no value leaks: {shown}");
    assert!(!shown.contains("/secret/bin"), "no value leaks: {shown}");
}

#[test]
fn refresh_publishes_a_new_environment_from_a_rewritten_body() {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "export MARK=1\nexec /bin/sh -c \"$4\"");
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(inherited(&shell, &temp.dir), &temp.home);
    first_wait(&environments, &clock, temp.diag(Arc::clone(&clock)));
    let old = environments.get();
    assert_eq!(get(&old.vars, "MARK").as_deref(), Some("1"));
    fakes::script(
        &temp.dir,
        "shell",
        &format!(
            "export FAKE_DIR={}\necho $$ > $FAKE_DIR/pid\nexport MARK=2\nexec /bin/sh -c \"$4\"",
            temp.dir.display(),
        ),
    );
    let new = refresh_wait(&environments, &clock, temp.diag(Arc::clone(&clock)));
    assert_eq!(get(&new.vars, "MARK").as_deref(), Some("2"));
    assert_eq!(new.source, VariablesSource::LoginShell);
    assert!(!Arc::ptr_eq(&old, &new), "a refresh publishes a new Arc");
    assert_eq!(environments.get().vars.len(), new.vars.len());
    let built: Vec<_> = temp
        .log_lines()
        .into_iter()
        .filter(|line| line["code"] == "environment_built")
        .collect();
    assert_eq!(built.len(), 2, "{built:?}");
    assert_eq!(built[1]["level"], "info");
}

#[test]
fn a_failed_refresh_publishes_the_fallback() {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "export MARK=1\nexec /bin/sh -c \"$4\"");
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(inherited(&shell, &temp.dir), &temp.home);
    first_wait(&environments, &clock, temp.diag(Arc::clone(&clock)));
    assert_eq!(environments.get().source, VariablesSource::LoginShell);
    fakes::script(
        &temp.dir,
        "shell",
        &format!(
            "export FAKE_DIR={}\necho $$ > $FAKE_DIR/pid\nexit 1\n",
            temp.dir.display(),
        ),
    );
    let new = refresh_wait(&environments, &clock, temp.diag(Arc::clone(&clock)));
    assert_eq!(new.source, VariablesSource::Inherited);
    assert!(get(&new.vars, "MARK").is_none());
    assert_eq!(
        get(&new.vars, "FIBER_HOME").as_deref(),
        Some(temp.home.to_string_lossy().as_ref())
    );
    let built: Vec<_> = temp
        .log_lines()
        .into_iter()
        .filter(|line| line["code"] == "environment_built")
        .collect();
    assert_eq!(built.len(), 2, "{built:?}");
    assert_eq!(built[1]["level"], "warn");
}

#[test]
fn two_refreshes_run_one_capture_at_a_time() {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "export MARK=1\nexec /bin/sh -c \"$4\"");
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(inherited(&shell, &temp.dir), &temp.home);
    first_wait(&environments, &clock, temp.diag(Arc::clone(&clock)));
    // The first refresh is held between its capture's end and its publish.
    fakes::script(
        &temp.dir,
        "shell",
        &format!(
            "export FAKE_DIR={}\necho $$ > $FAKE_DIR/pid\nexport MARK=2\nexec /bin/sh -c \"$4\"",
            temp.dir.display(),
        ),
    );
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    *support::lock(&environments.before_publish) = Some(Box::new(move || {
        held_tx.send(()).unwrap_or(());
        Deadline::after(WITHIN).recv(&release_rx).unwrap_or(());
    }));
    let first_env = Arc::clone(&environments);
    let first_clock = Arc::clone(&clock);
    let first_diag = temp.diag(Arc::clone(&clock));
    let (first_tx, first_rx) = mpsc::channel();
    thread::spawn(move || {
        first_tx
            .send(first_env.refresh(&*first_clock, &first_diag))
            .unwrap_or(());
    });
    Deadline::after(WITHIN)
        .recv(&held_rx)
        .expect("the first refresh holds before publishing");
    // The second refresh's body is rewritten now: it spawns only after the
    // first publishes, so its side file proves the serialization.
    let second_ran = temp.dir.join("second_ran");
    fakes::script(
        &temp.dir,
        "shell",
        &format!(
            "export FAKE_DIR={}\necho $$ > $FAKE_DIR/pid\ntouch {}\nexport MARK=3\nexec /bin/sh -c \"$4\"",
            temp.dir.display(),
            second_ran.display(),
        ),
    );
    fs::remove_file(&second_ran).unwrap_or(());
    // The second refresh signals its admission: it runs only after the
    // first holds the serializing lock, so the assertions below prove it
    // waits rather than passing because its thread never ran.
    let (admitted_tx, admitted_rx) = mpsc::channel();
    *support::lock(&environments.after_admit) = Some(Box::new(move || {
        admitted_tx.send(()).unwrap_or(());
    }));
    let second_env = Arc::clone(&environments);
    let second_clock = Arc::clone(&clock);
    let second_diag = temp.diag(Arc::clone(&clock));
    let (second_tx, second_rx) = mpsc::channel();
    thread::spawn(move || {
        second_tx
            .send(second_env.refresh(&*second_clock, &second_diag))
            .unwrap_or(());
    });
    Deadline::after(WITHIN)
        .recv(&admitted_rx)
        .expect("the second refresh is admitted");
    assert!(
        matches!(second_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the second refresh waits for the first"
    );
    assert!(
        !second_ran.exists(),
        "the second capture spawns only after the first published"
    );
    release_tx.send(()).unwrap_or(());
    let first = Deadline::after(WITHIN)
        .recv(&first_rx)
        .expect("the first refresh publishes");
    assert_eq!(get(&first.vars, "MARK").as_deref(), Some("2"));
    let second = Deadline::after(WITHIN)
        .recv(&second_rx)
        .expect("the second refresh publishes");
    assert_eq!(get(&second.vars, "MARK").as_deref(), Some("3"));
    assert!(second_ran.exists());
    assert_eq!(get(&environments.get().vars, "MARK").as_deref(), Some("3"));
}

#[test]
fn a_refresh_after_close_spawns_nothing_and_returns_the_published() {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "export MARK=1\nexec /bin/sh -c \"$4\"");
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(inherited(&shell, &temp.dir), &temp.home);
    first_wait(&environments, &clock, temp.diag(Arc::clone(&clock)));
    let published = environments.get();
    let lines_before = temp.log_lines().len();
    fs::remove_file(temp.dir.join("pid")).unwrap();
    environments.close();
    let after = refresh_wait(&environments, &clock, temp.diag(Arc::clone(&clock)));
    assert!(
        Arc::ptr_eq(&published, &after),
        "no new publish after close"
    );
    assert!(
        !temp.dir.join("pid").exists(),
        "the refresh after close spawned nothing"
    );
    assert_eq!(temp.log_lines().len(), lines_before, "no line after close");
}

#[test]
fn a_refresh_queued_on_the_lock_spawns_nothing_after_close() {
    let temp = Temp::new();
    let shell = fake_shell(&temp, "exec sleep 600");
    let clock = fakes::clock::FakeClock::new();
    let environments = Environments::new(inherited(&shell, &temp.dir), &temp.home);
    // The first capture holds the capture lock on its hung shell: the
    // refresh admitted now queues on it, so `close` lands between its
    // admission and its spawn.
    let first = Arc::clone(&environments);
    let first_clock = Arc::clone(&clock);
    let first_diag = temp.diag(Arc::clone(&clock));
    let first_done = thread::spawn(move || first.first(&*first_clock, &first_diag));
    let (pid, watchdog) = watchdog(&temp.dir);
    // The queued refresh's spawn would rewrite the pid file: remove it,
    // so any spawn after `close` is seen.
    fs::remove_file(temp.dir.join("pid")).unwrap();
    // Signal the refresh's admission while the first capture holds the
    // lock: `close` below lands between admission and spawn, forcing the
    // race instead of running entirely afterwards.
    let (admitted_tx, admitted_rx) = mpsc::channel();
    *support::lock(&environments.after_admit) = Some(Box::new(move || {
        admitted_tx.send(()).unwrap_or(());
    }));
    let second = Arc::clone(&environments);
    let second_clock = Arc::clone(&clock);
    let second_diag = temp.diag(Arc::clone(&clock));
    let (refresh_tx, refresh_rx) = mpsc::channel();
    thread::spawn(move || {
        refresh_tx
            .send(second.refresh(&*second_clock, &second_diag))
            .unwrap_or(());
    });
    Deadline::after(WITHIN)
        .recv(&admitted_rx)
        .expect("the refresh is admitted while the first capture holds the lock");
    environments.close();
    first_done.join().expect("the first capture ends");
    let refreshed = Deadline::after(WITHIN)
        .recv(&refresh_rx)
        .expect("the queued refresh returns");
    assert_eq!(refreshed.source, VariablesSource::Inherited);
    assert!(fakes::pids_exit(&[pid], WITHIN), "the shell is reaped");
    assert!(
        !temp.dir.join("pid").exists(),
        "the queued refresh spawned nothing"
    );
    assert!(
        temp.log_lines()
            .iter()
            .all(|line| line["code"] != "environment_built"),
        "a cancelled refresh writes no line"
    );
    watchdog.stand_down(WITHIN);
}

#[test]
fn user_home_answers_the_inherited_home() {
    let home = PathBuf::from("/Users/me");
    let environments = Environments::new(
        vec![
            (OsString::from("USER"), OsString::from("fiber-test")),
            (OsString::from("HOME"), home.as_os_str().to_owned()),
        ],
        Path::new("/tmp/hub-home"),
    );
    assert_eq!(environments.user_home(), Some(home));
}

#[test]
fn user_home_answers_the_first_home_when_inherited_twice() {
    let first = PathBuf::from("/first");
    let environments = Environments::new(
        vec![
            (OsString::from("HOME"), first.as_os_str().to_owned()),
            (OsString::from("HOME"), OsString::from("/second")),
        ],
        Path::new("/tmp/hub-home"),
    );
    assert_eq!(environments.user_home(), Some(first));
}

#[test]
fn user_home_is_none_without_an_inherited_home() {
    let environments = Environments::new(
        vec![(OsString::from("USER"), OsString::from("fiber-test"))],
        Path::new("/tmp/hub-home"),
    );
    assert_eq!(environments.user_home(), None);
}

#[test]
fn user_home_is_none_for_a_ready_environment() {
    let environments = Environments::ready(Environment {
        vars: vec![(OsString::from("HOME"), OsString::from("/Users/me"))],
        source: VariablesSource::Inherited,
    });
    assert_eq!(environments.user_home(), None);
}
