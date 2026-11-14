//! Recovery never runs against a live confined writer after keeper death
//! (`docs/tools.md`, "The keeper"): the confined command inherits the
//! active shared hold, so a recovery probe sees `WouldBlock` while the
//! writer lives, and restores only after its last process exits.

#[cfg(target_os = "linux")]
use std::fs::{File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Write as _;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt as _;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Child, ChildStdin, Command, Stdio};
#[cfg(target_os = "linux")]
use std::sync::mpsc;
#[cfg(target_os = "linux")]
use std::thread;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use fakes::children::Ready;

#[cfg(target_os = "linux")]
use super::super::output::OUTPUT_CAP;
#[cfg(target_os = "linux")]
use super::super::sandbox::protected::{Begin, ProtectedPaths, baseline};

#[allow(
    clippy::duplicate_mod,
    reason = "each unit-test file includes the shared support itself"
)]
#[cfg(target_os = "linux")]
#[path = "../../tests/support/sandbox_home.rs"]
mod sandbox_home;

/// One signal's bound: every wait names the signal or operation it awaits.
#[cfg(target_os = "linux")]
const WAIT: Duration = Duration::from_secs(10);
/// The outer bound for nested waits: twice the longest inner wait plus margin.
#[cfg(target_os = "linux")]
const DEADLINE: Duration = Duration::from_secs(30);

/// The keeper-death fixture entry: spawns the confined writer through the
/// production `keeper_spawn` seam and parks until killed. Started only by
/// the keeper-death tests below, never by a filter run.
#[test]
#[ignore = "started only by the keeper-death fixture"]
#[allow(
    clippy::print_stderr,
    reason = "the fixture reports a spawn failure where no test reads it"
)]
fn keeper_fixture() {
    let get =
        |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("the fixture needs {name}"));
    let baseline = PathBuf::from(get("FIBER_KEEPER_LOCK_BASELINE"));
    let fiber_home = PathBuf::from(get("FIBER_KEEPER_LOCK_FIBER_HOME"));
    let workspace = PathBuf::from(get("FIBER_KEEPER_LOCK_WORKSPACE"));
    let spec = contract::keeper::Spawn {
        id: "keeper-lock-fixture".to_owned(),
        program: PathBuf::from("/bin/sh"),
        args: vec![
            "-c".to_owned(),
            if get("FIBER_KEEPER_LOCK_MODE") == "grandchild" {
                grandchild_script(
                    &get("FIBER_KEEPER_LOCK_TOKEN"),
                    Path::new(&get("FIBER_KEEPER_LOCK_READY")),
                    Path::new(&get("FIBER_KEEPER_LOCK_RELEASE")),
                    Path::new(&get("FIBER_KEEPER_LOCK_TARGET")),
                )
            } else {
                writer_script(
                    &get("FIBER_KEEPER_LOCK_TOKEN"),
                    Path::new(&get("FIBER_KEEPER_LOCK_READY")),
                    Path::new(&get("FIBER_KEEPER_LOCK_RELEASE")),
                    Path::new(&get("FIBER_KEEPER_LOCK_TARGET")),
                )
            },
        ],
        cwd: workspace.clone(),
        env: Vec::new(),
        stdin: contract::keeper::HostStdin::Null,
        timeout_ms: None,
        output_cap: OUTPUT_CAP,
        sandbox: Some(contract::keeper::Sandbox {
            enabled: true,
            allow_write: Vec::new(),
            deny_read: Vec::new(),
            fiber_home,
            workspace,
            baseline: Some(baseline),
        }),
    };
    let (_child, _listing, _streams) = match crate::keeper_spawn(&spec) {
        Ok(spawned) => spawned,
        Err(error) => {
            eprintln!("the keeper fixture could not spawn its writer: {error}");
            std::process::exit(1);
        }
    };
    // Park until the parent kills this process: the writer outlives it in
    // its own process group. The streams stay open here, so the writer's
    // acknowledgement is never lost to a closed pipe.
    let mut stdin = std::io::stdin();
    let mut parked = [0u8; 1];
    use std::io::Read as _;
    let _ = stdin.read(&mut parked);
}

/// The confined writer: damages the protected file, acknowledges past the
/// damage, then blocks on the release FIFO the parent holds open.
#[cfg(target_os = "linux")]
fn writer_script(token: &str, ready: &Path, release: &Path, target: &Path) -> String {
    format!(
        "# {token}\necho $$ > {}\nprintf 'damaged' > {}\necho $$ >> {}\nread -r line < {}",
        quote(ready),
        quote(target),
        quote(ready),
        quote(release),
    )
}

/// The grandchild variant: the command leader damages nothing itself; it
/// reports its group, starts a separately executed descendant that damages,
/// acknowledges past its own exec, and holds the release FIFO, then exits.
#[cfg(target_os = "linux")]
fn grandchild_script(token: &str, ready: &Path, release: &Path, target: &Path) -> String {
    format!(
        "# {token}\necho $$ > {}\nsh -c 'printf \"damaged\" > \"{}\"; echo $$ >> \"{}\"; exec cat \"{}\" > /dev/null' &\nexit 0",
        quote(ready),
        quote(target),
        quote(ready),
        quote(release),
    )
}

/// A single-quoted shell path: the confined scripts never interpolate one.
#[cfg(target_os = "linux")]
fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

/// The forced keeper-death interleaving: a session guard, a keeper fixture
/// process holding a live confined writer, and the release FIFO that keeps
/// that writer alive independently of every pipe. One value owns every
/// process and pipe, so success, failure or panic releases or kills them.
#[cfg(target_os = "linux")]
struct KeeperDeath {
    work: fakes::TempDir,
    // Held so the sandbox home's root is swept only at test end.
    _settings_home: sandbox_home::SandboxHome,
    state_home: PathBuf,
    handle: std::sync::Arc<ProtectedPaths>,
    session: Option<Begin>,
    target: PathBuf,
    baseline: PathBuf,
    active: PathBuf,
    ready: Ready,
    release: Option<File>,
    keeper: Option<Child>,
    keeper_stdin: Option<ChildStdin>,
    keeper_id: u32,
    writer: Option<u32>,
    matching: Option<fakes::Watchdog>,
    group_watch: Option<fakes::Watchdog>,
}

#[cfg(target_os = "linux")]
impl KeeperDeath {
    /// Records the baseline, starts the keeper fixture for `mode`, and
    /// waits for the writer's acknowledgement past its damage.
    fn start(mode: &str) -> Self {
        let work = fakes::TempDir::new("fiber-keeper-lock-death");
        let fiber = work.path().join(".fiber");
        std::fs::create_dir_all(&fiber).expect("fiber dir created");
        std::fs::write(fiber.join("keep.txt"), b"original").expect("file written");
        let state_home = work.path().join("state-home");
        let (handle, startup) =
            ProtectedPaths::open(&state_home, work.path()).expect("open recovers");
        assert!(
            startup.is_none(),
            "a fresh project recovers nothing, got {startup:?}"
        );
        let session = Some(handle.begin(work.path()).expect("begin snapshots"));
        let settings_home = sandbox_home::SandboxHome::protected();
        let target = fiber.join("keep.txt");
        let dir = baseline::state_paths(&state_home, work.path()).expect("state paths");
        // The ready FIFO is read before the writer starts, so its
        // acknowledgement is never lost; the release FIFO is opened
        // read-write, so the open returns at once and the writer's own
        // open for reading succeeds, blocking only for the release bytes.
        let ready = Ready::new(work.path());
        let release_path = work.path().join("release.fifo");
        fakes::fifo(&release_path);
        let release = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&release_path)
            .expect("the release FIFO opens");
        let token = format!("fiber-keeper-lock-{}-{}", mode, std::process::id());
        let matching = fakes::Watchdog::matching(&token);
        let executable = std::env::current_exe().expect("the test executable");
        let mut keeper = Command::new(executable);
        keeper
            .arg("--ignored")
            .arg("--exact")
            .arg("shell::command::keeper_lock_tests::keeper_fixture")
            .arg("--nocapture")
            .env("FIBER_KEEPER_LOCK_BASELINE", &dir.baseline)
            .env("FIBER_KEEPER_LOCK_FIBER_HOME", settings_home.fiber_home())
            .env("FIBER_KEEPER_LOCK_WORKSPACE", work.path())
            .env("FIBER_KEEPER_LOCK_READY", ready.path())
            .env("FIBER_KEEPER_LOCK_RELEASE", &release_path)
            .env("FIBER_KEEPER_LOCK_TARGET", &target)
            .env("FIBER_KEEPER_LOCK_MODE", mode)
            .env("FIBER_KEEPER_LOCK_TOKEN", &token)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0);
        let mut keeper = keeper.spawn().expect("the keeper fixture starts");
        let keeper_id = keeper.id();
        let keeper_stdin = keeper.stdin.take();
        let mut death = Self {
            work,
            _settings_home: settings_home,
            state_home,
            handle,
            session,
            target,
            baseline: dir.baseline,
            active: dir.active_lock,
            ready,
            release: Some(release),
            keeper: Some(keeper),
            keeper_stdin,
            keeper_id,
            writer: None,
            matching: Some(matching),
            group_watch: None,
        };
        let group = death.ready.wait(fakes::Deadline::after(WAIT))[0];
        let group_watch = fakes::Watchdog::group(group);
        death.writer = Some(group);
        if let Some(matching) = death.matching.take() {
            matching.stand_down(fakes::Deadline::after(WAIT));
        }
        death.group_watch = Some(group_watch);
        // The second line arrives after the damage: the writer is past its
        // exec and, with the fix, holds the inherited active lock.
        let _ack = death.ready.wait(fakes::Deadline::after(WAIT));
        death
    }

    /// Drops the session guard without finishing, kills only the keeper
    /// fixture's group, and reaps the keeper. The writer's distinct group
    /// survives, still holding the release FIFO.
    fn kill_keeper(&mut self) {
        drop(self.session.take());
        assert!(
            support::group::alive(self.writer.expect("the writer is known")),
            "the writer lives before the keeper dies"
        );
        support::group::signal(self.keeper_id, rustix::process::Signal::KILL)
            .expect("the keeper dies");
        let mut keeper = self.keeper.take().expect("the keeper is known");
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let status = keeper.wait().expect("the keeper reaps");
            done.send(status).expect("the reap reports");
        });
        let status = fakes::Deadline::after(WAIT)
            .recv(&finished)
            .unwrap_or_else(|_| panic!("the keeper reaps"));
        assert!(
            status.code().is_none(),
            "the keeper died by signal, not by exit: {status}"
        );
        drop(self.keeper_stdin.take());
        assert!(
            support::group::alive(self.writer.expect("the writer is known")),
            "killing the keeper leaves the writer alive"
        );
    }

    /// Releases the writer past its FIFO hold and proves the last active
    /// holder is gone with a worker's exclusive take, released before any
    /// recovery runs.
    fn release_writer(&mut self) {
        if let Some(mut release) = self.release.take() {
            release
                .write_all(b"\n")
                .unwrap_or_else(|_| panic!("the release reaches the writer"));
        }
        let (done, released) = mpsc::channel();
        let active = self.active.clone();
        thread::spawn(move || {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&active)
                .expect("the lock opens");
            file.lock()
                .expect("the last holder released the active lock");
            file.unlock().expect("the probe releases before recovery");
            done.send(()).expect("the release reports");
        });
        fakes::Deadline::after(DEADLINE)
            .recv(&released)
            .unwrap_or_else(|_| panic!("the writer's exit released the active lock"));
        assert!(
            fakes::group_empties(
                self.writer.expect("the writer is known"),
                fakes::Deadline::after(WAIT)
            ),
            "the released writer leaves its group"
        );
    }
}

#[cfg(target_os = "linux")]
impl Drop for KeeperDeath {
    fn drop(&mut self) {
        drop(self.release.take());
        drop(self.session.take());
        if let Some(writer) = self.writer {
            let _ = support::group::signal(writer, rustix::process::Signal::KILL);
        }
        if self.keeper.is_some() {
            let _ = support::group::signal(self.keeper_id, rustix::process::Signal::KILL);
        }
    }
}

/// While the writer lives, startup recovery restores nothing and leaves the
/// protected bytes and the serialized baseline unchanged; once the writer
/// exits, it restores and reports, and a repeat recovers nothing.
#[cfg(target_os = "linux")]
#[test]
fn startup_recovery_waits_for_the_live_writer() {
    let mut death = KeeperDeath::start("simple");
    let baseline = std::fs::read(&death.baseline).expect("the baseline reads");
    death.kill_keeper();
    let (_, startup) =
        ProtectedPaths::open(&death.state_home, death.work.path()).expect("open probes recovery");
    assert!(
        startup.is_none(),
        "a live writer leaves startup recovery empty, got {startup:?}"
    );
    assert_eq!(
        std::fs::read(&death.target).expect("the target reads"),
        b"damaged",
        "startup recovery leaves the writer's bytes"
    );
    assert_eq!(
        std::fs::read(&death.baseline).expect("the baseline reads"),
        baseline,
        "startup recovery leaves the baseline fixed"
    );
    let probe = death.handle.begin(death.work.path()).expect("begin probes");
    assert!(
        probe.guard.recovered.is_empty(),
        "a live writer leaves begin recovery empty, got {:?}",
        probe.guard.recovered
    );
    assert_eq!(
        std::fs::read(&death.target).expect("the target reads"),
        b"damaged",
        "begin recovery leaves the writer's bytes"
    );
    assert_eq!(
        std::fs::read(&death.baseline).expect("the baseline reads"),
        baseline,
        "begin recovery leaves the baseline fixed"
    );
    drop(probe.guard);
    death.release_writer();
    let (_, startup) =
        ProtectedPaths::open(&death.state_home, death.work.path()).expect("open recovers");
    let report = startup.expect("a dead writer's baseline reports");
    assert_eq!(
        report.restored,
        vec![".fiber".to_owned()],
        "startup recovery names the restored path"
    );
    assert_eq!(
        std::fs::read(&death.target).expect("the target reads"),
        b"original",
        "startup recovery restores the writer's bytes"
    );
    assert!(
        !death.baseline.exists(),
        "startup recovery drops the baseline"
    );
    let (_, quiet) =
        ProtectedPaths::open(&death.state_home, death.work.path()).expect("open recovers");
    assert!(
        quiet.is_none(),
        "a dropped baseline recovers nothing, got {quiet:?}"
    );
    if let Some(watch) = death.group_watch.take() {
        watch.stand_down(fakes::Deadline::after(WAIT));
    }
}

/// While the writer lives, begin recovery recovers nothing; once the writer
/// exits, it recovers the stale baseline and snapshots fresh, so its end
/// reports nothing further.
#[cfg(target_os = "linux")]
#[test]
fn begin_recovery_waits_for_the_live_writer() {
    let mut death = KeeperDeath::start("simple");
    let baseline = std::fs::read(&death.baseline).expect("the baseline reads");
    death.kill_keeper();
    let probe = death.handle.begin(death.work.path()).expect("begin probes");
    assert!(
        probe.guard.recovered.is_empty(),
        "a live writer leaves begin recovery empty, got {:?}",
        probe.guard.recovered
    );
    assert_eq!(
        std::fs::read(&death.target).expect("the target reads"),
        b"damaged",
        "begin recovery leaves the writer's bytes"
    );
    assert_eq!(
        std::fs::read(&death.baseline).expect("the baseline reads"),
        baseline,
        "begin recovery leaves the baseline fixed"
    );
    drop(probe.guard);
    let (_, startup) =
        ProtectedPaths::open(&death.state_home, death.work.path()).expect("open probes recovery");
    assert!(
        startup.is_none(),
        "a live writer leaves startup recovery empty, got {startup:?}"
    );
    death.release_writer();
    let begin = death
        .handle
        .begin(death.work.path())
        .expect("begin recovers");
    assert_eq!(
        begin.guard.recovered.restored,
        vec![".fiber".to_owned()],
        "begin recovery names the restored path"
    );
    assert_eq!(
        std::fs::read(&death.target).expect("the target reads"),
        b"original",
        "begin recovery restores the writer's bytes"
    );
    assert!(
        death.baseline.exists(),
        "begin recovery snapshots a fresh baseline"
    );
    let report = begin.guard.finish();
    assert!(
        report.is_empty(),
        "the fresh snapshot leaves the end nothing to restore, got {report:?}"
    );
    if let Some(watch) = death.group_watch.take() {
        watch.stand_down(fakes::Deadline::after(WAIT));
    }
}

/// A separately executed descendant holds the recovery off after the
/// command leader exits: with the leader gone and the descendant past exec,
/// neither recovery entry restores, and the descendant's exit releases them.
#[cfg(target_os = "linux")]
#[test]
fn a_live_grandchild_holds_recovery_off_after_the_leader_exits() {
    let mut death = KeeperDeath::start("grandchild");
    let baseline = std::fs::read(&death.baseline).expect("the baseline reads");
    death.kill_keeper();
    let (_, startup) =
        ProtectedPaths::open(&death.state_home, death.work.path()).expect("open probes recovery");
    assert!(
        startup.is_none(),
        "a live descendant leaves startup recovery empty, got {startup:?}"
    );
    assert_eq!(
        std::fs::read(&death.target).expect("the target reads"),
        b"damaged",
        "startup recovery leaves the descendant's bytes"
    );
    assert_eq!(
        std::fs::read(&death.baseline).expect("the baseline reads"),
        baseline,
        "startup recovery leaves the baseline fixed"
    );
    let probe = death.handle.begin(death.work.path()).expect("begin probes");
    assert!(
        probe.guard.recovered.is_empty(),
        "a live descendant leaves begin recovery empty, got {:?}",
        probe.guard.recovered
    );
    drop(probe.guard);
    death.release_writer();
    let (_, startup) =
        ProtectedPaths::open(&death.state_home, death.work.path()).expect("open recovers");
    let report = startup.expect("the descendant's exit releases recovery");
    assert_eq!(
        report.restored,
        vec![".fiber".to_owned()],
        "startup recovery names the restored path"
    );
    assert_eq!(
        std::fs::read(&death.target).expect("the target reads"),
        b"original",
        "startup recovery restores the descendant's bytes"
    );
    if let Some(watch) = death.group_watch.take() {
        watch.stand_down(fakes::Deadline::after(WAIT));
    }
}
