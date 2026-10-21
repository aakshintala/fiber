//! Tests for the login-shell capture (`docs/invocation.md`, "A session's
//! environment"). Every fake shell is [`fakes::script`] in a
//! [`fakes::TempDir`]; every capture runs on a thread and its result is
//! received with a deadline, so a hang fails the test instead of hanging
//! it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use super::{
    CAPTURE_DEADLINE, Cancel, Failed, OUTPUT_CAP, capture, marker_suffix, parse, read_all,
    shell_command, wait,
};
use fakes::Deadline;

/// How long a wait that must succeed holds, on the wall clock.
const WITHIN: Duration = Duration::from_secs(10);

/// How long a wait that must see nothing holds: several of the capture's
/// wall-clock polls.
const QUIET: Duration = Duration::from_millis(300);

/// The begin marker's bytes for `nonce`, as the capture matches them.
fn begin(nonce: &str) -> Vec<u8> {
    format!("FIBER_ENV_BEGIN_{nonce}\n").into_bytes()
}

/// The end marker's bytes for `nonce`, as the capture matches them.
fn end(nonce: &str) -> Vec<u8> {
    format!("FIBER_ENV_END_{nonce}\n").into_bytes()
}

/// Output holding `region` between both markers for `nonce`.
fn framed(nonce: &str, chatter_before: &[u8], region: &[u8], chatter_after: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(chatter_before);
    out.extend_from_slice(&begin(nonce));
    out.extend_from_slice(region);
    out.extend_from_slice(&end(nonce));
    out.extend_from_slice(chatter_after);
    out
}

/// One `NAME=value` entry, NUL-terminated.
fn entry(name: &[u8], value: &[u8]) -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend_from_slice(name);
    entry.push(b'=');
    entry.extend_from_slice(value);
    entry.push(0);
    entry
}

#[test]
fn chatter_before_the_begin_marker_is_ignored() {
    let region = entry(b"A", b"1");
    let out = framed("n1", b"startup chatter\n", &region, b"");
    assert_eq!(
        parse(&out, "n1").unwrap(),
        vec![(OsString::from("A"), OsString::from("1"))]
    );
}

#[test]
fn chatter_after_the_end_marker_is_ignored() {
    let region = entry(b"A", b"1");
    let out = framed("n1", b"", &region, b"a daemon holds stdout\n");
    assert_eq!(
        parse(&out, "n1").unwrap(),
        vec![(OsString::from("A"), OsString::from("1"))]
    );
}

#[test]
fn a_value_holding_equals_and_newline_is_kept() {
    let region = entry(b"A", b"x=y\nz");
    let out = framed("n1", b"", &region, b"");
    assert_eq!(
        parse(&out, "n1").unwrap(),
        vec![(OsString::from("A"), OsString::from("x=y\nz"))]
    );
}

#[test]
fn an_empty_region_is_a_valid_empty_environment() {
    let out = framed("n1", b"", b"", b"");
    assert_eq!(parse(&out, "n1").unwrap(), Vec::new());
}

#[test]
fn no_begin_marker_is_malformed() {
    let mut out = Vec::new();
    out.extend_from_slice(&end("n1"));
    assert_eq!(parse(&out, "n1"), Err(Failed::Malformed));
}

#[test]
fn a_begin_without_an_end_is_malformed() {
    let mut out = Vec::new();
    out.extend_from_slice(&begin("n1"));
    out.extend_from_slice(&entry(b"A", b"1"));
    assert_eq!(parse(&out, "n1"), Err(Failed::Malformed));
}

#[test]
fn an_entry_without_equals_is_malformed() {
    let mut region = Vec::new();
    region.extend_from_slice(b"NOEQUALS");
    region.push(0);
    let out = framed("n1", b"", &region, b"");
    assert_eq!(parse(&out, "n1"), Err(Failed::Malformed));
}

#[test]
fn an_empty_name_is_malformed() {
    let region = entry(b"", b"x");
    let out = framed("n1", b"", &region, b"");
    assert_eq!(parse(&out, "n1"), Err(Failed::Malformed));
}

#[test]
fn a_region_not_nul_terminated_is_malformed() {
    let mut region = Vec::new();
    region.extend_from_slice(b"A=1");
    let out = framed("n1", b"", &region, b"");
    assert_eq!(parse(&out, "n1"), Err(Failed::Malformed));
}

#[test]
fn a_begin_marker_with_the_wrong_nonce_is_malformed() {
    let region = entry(b"A", b"1");
    let out = framed("other", b"", &region, b"");
    assert_eq!(parse(&out, "n1"), Err(Failed::Malformed));
}

#[test]
fn the_begin_marker_prefix_without_the_nonce_is_chatter() {
    let region = entry(b"A", b"1");
    let out = framed("n1", b"FIBER_ENV_BEGIN_\n", &region, b"");
    assert_eq!(
        parse(&out, "n1").unwrap(),
        vec![(OsString::from("A"), OsString::from("1"))]
    );
}

#[test]
fn shell_command_holds_only_the_base_with_login_args() {
    let base = vec![
        (OsString::from("HOME"), OsString::from("/Users/me")),
        (OsString::from("USER"), OsString::from("me")),
        (OsString::from("SHELL"), OsString::from("/bin/zsh")),
    ];
    let (ours, theirs) = UnixStream::pair().unwrap();
    let theirs: std::os::unix::io::OwnedFd = theirs.into();
    let command = shell_command(Path::new("/bin/zsh"), &base, "echo hi", Stdio::from(theirs));
    drop(ours);
    let envs: Vec<(OsString, Option<OsString>)> = command
        .get_envs()
        .map(|(name, value)| (name.to_owned(), value.map(|value| value.to_owned())))
        .collect();
    assert_eq!(envs.len(), 3, "{envs:?}");
    for (name, value) in &base {
        assert!(
            envs.contains(&(name.clone(), Some(value.clone()))),
            "{envs:?} holds {name:?}"
        );
    }
    let args: Vec<OsString> = command.get_args().map(|arg| arg.to_owned()).collect();
    assert_eq!(args, vec!["-l", "-i", "-c", "echo hi"]);
}

#[test]
fn shell_command_without_user_holds_only_home_and_shell() {
    let base = vec![
        (OsString::from("HOME"), OsString::from("/Users/me")),
        (OsString::from("SHELL"), OsString::from("/bin/zsh")),
    ];
    let (ours, theirs) = UnixStream::pair().unwrap();
    let theirs: std::os::unix::io::OwnedFd = theirs.into();
    let command = shell_command(Path::new("/bin/zsh"), &base, "echo hi", Stdio::from(theirs));
    drop(ours);
    let names: Vec<OsString> = command
        .get_envs()
        .map(|(name, _)| name.to_owned())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&OsString::from("HOME")));
    assert!(names.contains(&OsString::from("SHELL")));
}

/// A fake-shell directory: the script, the pid file it writes, and the
/// side files bodies report through.
struct Shell {
    /// Holds the directory: drop kills the group.
    _held: fakes::TempDir,
    /// The directory.
    dir: PathBuf,
    /// The fake `$SHELL`.
    shell: PathBuf,
}

impl Shell {
    /// Writes `body` as the fake shell. The body's first lines report its
    /// pid and the check-run nonce, so the test can arm its watchdog.
    fn new(body: &str) -> Self {
        let held = fakes::TempDir::new("hc");
        let dir = held.path().to_path_buf();
        let shell = fakes::script(&dir, "shell", &prologue(&dir, body));
        Self {
            _held: held,
            dir,
            shell,
        }
    }

    /// The inherited environment handing `shell` as `$SHELL`.
    fn inherited(&self) -> Vec<(OsString, OsString)> {
        vec![
            (OsString::from("HOME"), self.dir.as_os_str().to_owned()),
            (OsString::from("USER"), OsString::from("fiber-test")),
            (OsString::from("SHELL"), self.shell.as_os_str().to_owned()),
        ]
    }

    /// The shell's pid, read under a wall-clock deadline, with the
    /// watchdog killing its group on drop armed: the shell leads its own
    /// session and group, so the pid is the group.
    fn watchdog(&self) -> fakes::Watchdog {
        fakes::Watchdog::group(await_pid(&self.dir))
    }

    /// The shell's pid, read under a wall-clock deadline.
    fn pid(&self) -> u32 {
        await_pid(&self.dir)
    }

    /// Reads the side file `name` the body wrote.
    fn side(&self, name: &str) -> String {
        fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }
}

/// `body` with the pid and nonce prologue first. The prologue exports
/// `FAKE_DIR`, so bodies report through `"$FAKE_DIR/<name>"`.
fn prologue(dir: &Path, body: &str) -> String {
    let nonce = match fakes::check_run() {
        Some((name, value)) => format!("export {name}={}\n", value.to_string_lossy()),
        None => String::new(),
    };
    format!(
        "export FAKE_DIR={}\necho $$ > $FAKE_DIR/pid\n{} {}",
        dir.display(),
        nonce,
        body
    )
}

/// The shell's pid, read under a wall-clock deadline.
fn await_pid(dir: &Path) -> u32 {
    let deadline = Deadline::after(WITHIN);
    loop {
        if let Ok(text) = fs::read_to_string(dir.join("pid"))
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            return pid;
        }
        if deadline.left().is_zero() {
            panic!("waited for the fake shell to report its pid");
        }
        thread::yield_now();
    }
}

/// Runs `capture` on a thread: the result arrives with a deadline, so a
/// hang fails the test instead of hanging it.
fn capture_on_thread(
    inherited: Vec<(OsString, OsString)>,
    clock: Arc<fakes::clock::FakeClock>,
    cancel: Arc<Cancel>,
) -> mpsc::Receiver<Result<Vec<(OsString, OsString)>, Failed>> {
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let outcome = capture(&inherited, &*clock, &cancel);
        done_tx.send(outcome).unwrap_or(());
    });
    done_rx
}

/// The capture's result, failing the test past the deadline.
fn outcome_of(
    rx: &mpsc::Receiver<Result<Vec<(OsString, OsString)>, Failed>>,
    what: &str,
) -> Result<Vec<(OsString, OsString)>, Failed> {
    Deadline::after(WITHIN).recv(rx).unwrap_or_else(|_| {
        panic!("waited for {what}");
    })
}

/// The vars as `NAME=value` strings, for assertions without whole-value
/// comparison.
fn get(vars: &[(OsString, OsString)], name: &str) -> Option<String> {
    vars.iter()
        .find_map(|(n, v)| (n == name).then(|| v.to_string_lossy().into_owned()))
}

/// The whole process group is dead, then the watchdog stands down: the
/// capture kills only the shell pid, so a `sleep 600 &` descendant
/// outlives it carrying the check-run nonce. Standing down with the group
/// alive would leak it past the test into the leak scan.
fn reap_group(pid: u32, watchdog: fakes::Watchdog) {
    fakes::kill_group(pid, "KILL").unwrap();
    assert!(
        fakes::group_empties(pid, fakes::Deadline::after(WITHIN)),
        "the held group is reaped"
    );
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
}

#[test]
fn a_successful_capture_sees_only_the_base_and_runs_in_home() {
    let fake = Shell::new(
        r#"printf '%s\n' "$1 $2 $3" > "$FAKE_DIR/args"
printf '%s\n' "${HOME-unset} ${USER-unset} ${SHELL-unset}" > "$FAKE_DIR/inputs"
printf '%s\n' "${HUB_ONLY-unset}" > "$FAKE_DIR/hub_only"
printf '%s\n' "${CARGO_MANIFEST_DIR-unset}" > "$FAKE_DIR/manifest"
pwd > "$FAKE_DIR/pwd"
export FROM_PROFILE=1
exec /bin/sh -c "$4""#,
    );
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    let vars = outcome_of(&done, "the successful capture").expect("the capture succeeds");
    let pid = fake.pid();
    assert!(
        fakes::pids_exit(&[pid], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    assert_eq!(fake.side("args").trim(), "-l -i -c");
    let home = fake.dir.to_string_lossy().into_owned();
    assert_eq!(
        fake.side("inputs").trim(),
        format!("{home} fiber-test {}", fake.shell.to_string_lossy())
    );
    assert_eq!(fake.side("hub_only").trim(), "unset");
    assert_eq!(fake.side("manifest").trim(), "unset");
    // The shell starts in `HOME`: `pwd` is physical, so the directory is
    // canonicalized before comparing.
    let physical = fs::canonicalize(&fake.dir)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(fake.side("pwd").trim(), physical);
    assert_eq!(get(&vars, "FROM_PROFILE").as_deref(), Some("1"));
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
}

#[test]
fn a_missing_home_still_captures() {
    let fake = Shell::new(
        r#"export FROM_PROFILE=1
exec /bin/sh -c "$4""#,
    );
    let mut inherited = fake.inherited();
    let missing = fake.dir.join("no-such-home");
    for (name, value) in &mut inherited {
        if name == "HOME" {
            *value = missing.as_os_str().to_owned();
        }
    }
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let done = capture_on_thread(inherited, Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    let vars = outcome_of(&done, "the capture with a missing HOME").expect("still succeeds");
    assert!(
        fakes::pids_exit(&[fake.pid()], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    assert_eq!(get(&vars, "FROM_PROFILE").as_deref(), Some("1"));
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
}

/// A capture with no shell to spawn: nothing runs, and the failure names
/// its class exactly.
fn failed_without_spawn(inherited: Vec<(OsString, OsString)>) -> Failed {
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let done = capture_on_thread(inherited, Arc::clone(&clock), Arc::clone(&cancel));
    outcome_of(&done, "the failed capture").expect_err("the capture fails")
}

#[test]
fn an_unset_shell_is_no_shell_without_spawning() {
    let fake = Shell::new("exit 0");
    let inherited: Vec<(OsString, OsString)> = fake
        .inherited()
        .into_iter()
        .filter(|(name, _)| name != "SHELL")
        .collect();
    assert_eq!(failed_without_spawn(inherited), Failed::NoShell);
    assert!(!fake.dir.join("pid").exists(), "no shell spawned");
}

#[test]
fn an_empty_shell_is_no_shell_without_spawning() {
    let fake = Shell::new("exit 0");
    let mut inherited = fake.inherited();
    for (name, value) in &mut inherited {
        if name == "SHELL" {
            *value = OsString::new();
        }
    }
    assert_eq!(failed_without_spawn(inherited), Failed::NoShell);
    assert!(!fake.dir.join("pid").exists(), "no shell spawned");
}

#[test]
fn a_relative_shell_is_not_absolute_without_spawning() {
    let fake = Shell::new("exit 0");
    let mut inherited = fake.inherited();
    for (name, value) in &mut inherited {
        if name == "SHELL" {
            *value = OsString::from("zsh");
        }
    }
    assert_eq!(failed_without_spawn(inherited), Failed::NotAbsolute);
    assert!(!fake.dir.join("pid").exists(), "no shell spawned");
}

#[test]
fn a_missing_shell_is_a_spawn_failure() {
    let fake = Shell::new("exit 0");
    let mut inherited = fake.inherited();
    for (name, value) in &mut inherited {
        if name == "SHELL" {
            *value = OsString::from("/nonexistent/shell");
        }
    }
    assert_eq!(
        failed_without_spawn(inherited),
        Failed::Spawn(std::io::ErrorKind::NotFound)
    );
    assert!(!fake.dir.join("pid").exists(), "no shell spawned");
}

#[test]
fn a_shell_that_exits_before_printing_is_exited() {
    let fake = Shell::new("exit 3");
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    assert_eq!(
        outcome_of(&done, "the exiting capture"),
        Err(Failed::Exited)
    );
    assert!(
        fakes::pids_exit(&[fake.pid()], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
}

#[test]
fn a_begin_without_an_end_is_exited() {
    // The body prints only the script's first half: the begin marker
    // without the end marker, then exits.
    let fake = Shell::new(
        r#"eval "${4%%;*}"
exit 0"#,
    );
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    assert_eq!(
        outcome_of(&done, "the half-printed capture"),
        Err(Failed::Exited)
    );
    assert!(
        fakes::pids_exit(&[fake.pid()], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
}

#[test]
fn output_past_the_cap_is_too_large() {
    // The begin marker, then 5 MiB of spaces in bounded 64 KiB chunks,
    // then a hang: the capture ends on the cap, never on the exit.
    let fake = Shell::new(
        r#"eval "${4%%;*}"
i=0
while [ $i -lt 80 ]; do printf '%65536s' ' '; i=$((i+1)); done
exec sleep 600"#,
    );
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    assert_eq!(
        outcome_of(&done, "the oversized capture"),
        Err(Failed::TooLarge)
    );
    assert!(
        fakes::pids_exit(&[fake.pid()], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
}

#[test]
fn a_missing_env_prints_begin_but_no_end() {
    // The script with `/usr/bin/env` rewritten to a missing path: the
    // begin marker prints, `env` fails, and no end marker follows.
    let fake = Shell::new(
        r#"exec /bin/sh -c "$(printf '%s' "$4" | /usr/bin/sed 's#/usr/bin/env#/nonexistent/env#')""#,
    );
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    assert_eq!(
        outcome_of(&done, "the capture without env"),
        Err(Failed::Exited)
    );
    assert!(
        fakes::pids_exit(&[fake.pid()], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
}

#[test]
fn a_hanging_shell_times_out_on_the_fake_clock() {
    let fake = Shell::new("exec sleep 600");
    let clock = fakes::clock::FakeClock::new();
    let origin = clock.origin();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    clock.advance_when_parked(
        Some(origin + CAPTURE_DEADLINE),
        None,
        CAPTURE_DEADLINE,
        &Deadline::after(WITHIN),
    );
    assert_eq!(
        outcome_of(&done, "the hanging capture"),
        Err(Failed::TimedOut)
    );
    assert!(
        fakes::pids_exit(&[fake.pid()], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
}

#[test]
fn the_deadline_fires_at_or_after_ten_seconds() {
    let fake = Shell::new("exec sleep 600");
    let clock = fakes::clock::FakeClock::new();
    let origin = clock.origin();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    clock.advance_when_parked(
        Some(origin + CAPTURE_DEADLINE),
        None,
        CAPTURE_DEADLINE
            .checked_sub(Duration::from_millis(1))
            .expect("ten seconds minus a millisecond"),
        &Deadline::after(WITHIN),
    );
    assert!(
        Deadline::after(QUIET).recv(&done).is_err(),
        "no result before the deadline"
    );
    clock.advance_when_parked(
        Some(origin + CAPTURE_DEADLINE),
        None,
        Duration::from_millis(1),
        &Deadline::after(WITHIN),
    );
    assert_eq!(
        outcome_of(&done, "the hanging capture"),
        Err(Failed::TimedOut)
    );
    assert!(
        fakes::pids_exit(&[fake.pid()], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
}

#[test]
fn a_success_is_killed_and_reaped() {
    // The framed output, then a hang: the capture ends on the end marker
    // and still reaps the shell.
    let fake = Shell::new(
        r#"export FROM_PROFILE=1
exec /bin/sh -c "$4; exec sleep 600""#,
    );
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    let vars = outcome_of(&done, "the succeeding capture").expect("the capture succeeds");
    assert_eq!(get(&vars, "FROM_PROFILE").as_deref(), Some("1"));
    assert!(
        fakes::pids_exit(&[fake.pid()], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
}

#[test]
fn cancelling_a_hung_capture_kills_and_reaps() {
    let fake = Shell::new("exec sleep 600");
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    let pid = fake.pid();
    cancel.cancel();
    assert_eq!(
        outcome_of(&done, "the cancelled capture"),
        Err(Failed::Cancelled)
    );
    assert!(
        fakes::pids_exit(&[pid], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
}

#[test]
fn cancelling_with_a_held_output_still_returns() {
    // A descendant holds the shell's end of the stdout socket: the stop
    // still ends the reader, so the capture returns only after the join.
    let fake = Shell::new("sleep 600 &\nexec sleep 600");
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    let pid = fake.pid();
    cancel.cancel();
    assert_eq!(
        outcome_of(&done, "the cancelled capture"),
        Err(Failed::Cancelled)
    );
    assert!(
        fakes::pids_exit(&[pid], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    reap_group(pid, watchdog);
}

#[test]
fn a_timeout_with_a_held_output_still_returns() {
    let fake = Shell::new("sleep 600 &\nexec sleep 600");
    let clock = fakes::clock::FakeClock::new();
    let origin = clock.origin();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    clock.advance_when_parked(
        Some(origin + CAPTURE_DEADLINE),
        None,
        CAPTURE_DEADLINE,
        &Deadline::after(WITHIN),
    );
    assert_eq!(
        outcome_of(&done, "the hanging capture"),
        Err(Failed::TimedOut)
    );
    let pid = fake.pid();
    assert!(
        fakes::pids_exit(&[pid], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    reap_group(pid, watchdog);
}

#[test]
fn a_success_with_a_held_output_still_returns() {
    // The framed output, then an exit leaving a descendant holding the
    // socket: the end marker ends the capture, not the end of output.
    let fake = Shell::new("sleep 600 &\nexec /bin/sh -c \"$4\"");
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    outcome_of(&done, "the succeeding capture").expect("the capture succeeds");
    let pid = fake.pid();
    assert!(
        fakes::pids_exit(&[pid], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    reap_group(pid, watchdog);
}

/// `is_cancelled` reports cancellation, in both polarities. The
/// capture-level test cannot pin the `false` mutant: a pre-cancelled
/// capture still reports `Cancelled` through the wait's own flag check,
/// and the shell it spawns under the mutant is killed before its pid file
/// could witness the spawn.
#[test]
fn cancel_reports_cancellation() {
    let cancel = Cancel::new();
    assert!(!cancel.is_cancelled(), "a fresh capture is not cancelled");
    cancel.cancel();
    assert!(cancel.is_cancelled(), "cancelling is observed");
}

#[test]
fn an_already_cancelled_capture_fails_without_spawning() {
    let fake = Shell::new("exit 0");
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    cancel.cancel();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    assert_eq!(
        outcome_of(&done, "the cancelled capture"),
        Err(Failed::Cancelled)
    );
    assert!(!fake.dir.join("pid").exists(), "no shell spawned");
}

/// A megabyte-scale environment under the cap: one hundred 16 KiB values,
/// past every shrunk cap arithmetic and under the 4 MiB one. Single values
/// stay small because one environment string past 128 KiB cannot cross
/// `execve` on Linux. Linux only: macOS `ARG_MAX` is 1 MiB, which cannot
/// hold this environment at all.
#[cfg(target_os = "linux")]
const PAD_VARS: usize = 100;
#[cfg(target_os = "linux")]
const PAD_LEN: usize = 16_384;

#[test]
#[cfg(target_os = "linux")]
fn a_large_environment_under_the_cap_succeeds() {
    let fake = Shell::new(&format!(
        "i=0\nwhile [ $i -lt {PAD_VARS} ]; do eval \"export PAD_$i=\\\"$(printf '%{PAD_LEN}s' ' ')\\\"\"; i=$((i+1)); done\nexec /bin/sh -c \"$4\""
    ));
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    let vars = outcome_of(&done, "the large capture").expect("the capture succeeds");
    assert!(
        fakes::pids_exit(&[fake.pid()], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    let pads: Vec<&(OsString, OsString)> = vars
        .iter()
        .filter(|(name, _)| name.to_string_lossy().starts_with("PAD_"))
        .collect();
    assert_eq!(pads.len(), PAD_VARS, "every pad value captured");
    for (_, value) in pads {
        assert_eq!(value.to_string_lossy().len(), PAD_LEN);
    }
    watchdog.stand_down(fakes::Deadline::after(WITHIN));
}

#[test]
fn a_cap_sized_output_without_a_marker_parks() {
    // Exactly `OUTPUT_CAP` bytes, no markers, then a hang: at the cap the
    // wait parks (past the cap is `TooLarge`), so this pins the edge's
    // exact side. The scanned signal proves the wait processed every byte:
    // the shell's side file only proves writing finished, not that the
    // capture saw it. The bounded wait is several of the capture's own
    // 20 ms polls, then the test cancels and requires the answer.
    let fake = Shell::new(&format!(
        "printf '%{OUTPUT_CAP}s' ' '; echo printed > \"$FAKE_DIR/printed\"\nexec sleep 600"
    ));
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let done = capture_on_thread(fake.inherited(), Arc::clone(&clock), Arc::clone(&cancel));
    let watchdog = fake.watchdog();
    // Signal: the wait scanned the whole cap, so it decided to park rather
    // than still transferring.
    let scanned = Deadline::after(WITHIN);
    loop {
        let seen = cancel.state.lock().unwrap().scanned;
        if seen >= OUTPUT_CAP {
            break;
        }
        if scanned.left().is_zero() {
            panic!("waited for the capture to scan the cap");
        }
        thread::yield_now();
    }
    assert!(
        Deadline::after(QUIET).recv(&done).is_err(),
        "no result at exactly the cap"
    );
    cancel.cancel();
    assert_eq!(
        outcome_of(&done, "the cancelled capture"),
        Err(Failed::Cancelled)
    );
    let pid = fake.pid();
    assert!(
        fakes::pids_exit(&[pid], fakes::Deadline::after(WITHIN)),
        "the shell is reaped"
    );
    reap_group(pid, watchdog);
}

/// `output` through `wait` with no deadline and the reader ended (unless
/// noted): the result is immediate, so no thread or clock advance is
/// needed. Only the small result is compared, never the megabytes of input
/// (`docs/testing.md`, "A test on a large value").
fn wait_once(
    output: Vec<u8>,
    reader_done: bool,
    nonce: &str,
) -> Result<Vec<(OsString, OsString)>, Failed> {
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    {
        let mut state = cancel.state.lock().unwrap();
        state.output = output;
        state.reader_done = reader_done;
    }
    wait(&cancel, &*clock, None, nonce)
}

/// `nonce` framed around an empty region, with `before` chatter bytes ahead
/// of the begin marker and `after` chatter past the end marker: the bytes
/// before the end marker are `before + begin.len()`.
fn framed_sizes(nonce: &str, before: usize, after: usize) -> Vec<u8> {
    let mut out = vec![b'x'; before];
    out.extend_from_slice(&begin(nonce));
    out.extend_from_slice(&end(nonce));
    out.extend_from_slice(&vec![b'y'; after]);
    out
}

#[test]
fn one_below_the_cap_before_the_end_marker_succeeds() {
    let before = OUTPUT_CAP - 1 - begin("n1").len();
    let result = wait_once(framed_sizes("n1", before, 0), true, "n1");
    assert_eq!(result, Ok(Vec::new()));
}

#[test]
fn exactly_the_cap_before_the_end_marker_succeeds() {
    let before = OUTPUT_CAP - begin("n1").len();
    let result = wait_once(framed_sizes("n1", before, 0), true, "n1");
    assert_eq!(result, Ok(Vec::new()));
}

#[test]
fn one_past_the_cap_before_the_end_marker_is_too_large() {
    let before = OUTPUT_CAP + 1 - begin("n1").len();
    let result = wait_once(framed_sizes("n1", before, 0), true, "n1");
    assert_eq!(result, Err(Failed::TooLarge));
}

#[test]
fn chatter_after_the_end_marker_ignores_the_cap() {
    // A valid capture with a small prefix, followed by more than the cap
    // of descendant chatter: the cap covers only bytes before the end
    // marker, so this succeeds. Under the total-bytes check it reports
    // `TooLarge`.
    let result = wait_once(framed_sizes("n1", 0, OUTPUT_CAP + 1), true, "n1");
    assert_eq!(result, Ok(Vec::new()));
}

#[test]
fn one_past_the_cap_without_a_marker_is_too_large() {
    // No marker at all: the whole buffer counts, minus no split tail, so
    // one past the cap fails. A `marker_suffix` stuck at 1 parks here
    // instead (then `Exited` on the ended reader), so this pins the zero
    // case.
    let output = vec![b'x'; OUTPUT_CAP + 1];
    assert_eq!(wait_once(output, true, "n1"), Err(Failed::TooLarge));
}

#[test]
fn marker_suffix_reports_a_split_marker_tail() {
    let nonce = "suffix";
    let marker = end(nonce);
    assert_eq!(marker_suffix(b"hello", &marker), 0);
    assert_eq!(marker_suffix(b"", &marker), 0);
    assert_eq!(marker_suffix(b"xyz", &marker), 0);
    let mut partial = b"hello".to_vec();
    partial.extend_from_slice(marker.get(..3).expect("the marker is longer than 3"));
    assert_eq!(marker_suffix(&partial, &marker), 3);
    let mut max = b"hello".to_vec();
    max.extend_from_slice(
        marker
            .get(..marker.len() - 1)
            .expect("the marker is longer than 1"),
    );
    assert_eq!(marker_suffix(&max, &marker), marker.len() - 1);
    assert_eq!(
        marker_suffix(
            marker.get(..2).expect("the marker is longer than 2"),
            &marker
        ),
        2
    );
    let mut one = b"xyz".to_vec();
    one.push(*marker.first().expect("the marker is not empty"));
    assert_eq!(marker_suffix(&one, &marker), 1);
    // A periodic needle: the longest tail wins, so the reverse order is
    // pinned, not the first match.
    assert_eq!(marker_suffix(b"XAAAA", b"AAAA"), 3);
}

/// Runs `wait` on a thread and feeds it `first`, waiting until it scanned
/// every byte before returning the receiver: the next chunk is really a
/// later read, so a marker split across the two is split across reads.
#[allow(clippy::type_complexity, reason = "one test-only thread handle")]
fn wait_with_first_chunk(
    first: Vec<u8>,
    nonce: &str,
) -> (
    Arc<Cancel>,
    mpsc::Receiver<Result<Vec<(OsString, OsString)>, Failed>>,
) {
    let clock = fakes::clock::FakeClock::new();
    let cancel = Cancel::new();
    let (done_tx, done_rx) = mpsc::channel();
    let moved = Arc::clone(&cancel);
    let owned = nonce.to_owned();
    thread::spawn(move || {
        let outcome = wait(&moved, &*clock, None, &owned);
        done_tx.send(outcome).unwrap_or(());
    });
    {
        let mut state = cancel.state.lock().unwrap();
        state.output = first;
    }
    cancel.changed.notify_all();
    (cancel, done_rx)
}

/// Waits until the wait scanned at least `scanned` bytes, proving it saw
/// the fed chunk and decided to park rather than still transferring.
fn await_scanned(cancel: &Cancel, scanned: usize) {
    let deadline = Deadline::after(WITHIN);
    loop {
        if cancel.state.lock().unwrap().scanned >= scanned {
            return;
        }
        if deadline.left().is_zero() {
            panic!("waited for the wait to scan {scanned} bytes");
        }
        thread::yield_now();
    }
}

#[test]
fn split_end_marker_across_reads_at_the_cap_boundary() {
    // A valid prefix at cap-1 and at the cap, followed by the end marker
    // split across two reads, still succeeds: only bytes before the
    // marker count, and the first half waiting in the buffer is marker,
    // not output. One past the cap still fails on its first half alone.
    // Without the suffix the at-cap case reports `TooLarge` as soon as
    // the first half lands.
    let nonce = "splitcap";
    let marker = end(nonce);
    let split = marker.len() / 2;
    assert!(split >= 1 && split < marker.len());
    for (prefix_len, expect_ok) in [
        (OUTPUT_CAP - 1, true),
        (OUTPUT_CAP, true),
        (OUTPUT_CAP + 1, false),
    ] {
        let padding = prefix_len - begin(nonce).len();
        let mut first = vec![b'x'; padding];
        first.extend_from_slice(&begin(nonce));
        assert_eq!(first.len(), prefix_len);
        first.extend_from_slice(marker.get(..split).expect("split is in the marker"));
        let first_len = first.len();
        let (cancel, done) = wait_with_first_chunk(first, nonce);
        await_scanned(&cancel, first_len);
        if expect_ok {
            assert!(
                Deadline::after(QUIET).recv(&done).is_err(),
                "no result on the split half at prefix {prefix_len}"
            );
            {
                let mut state = cancel.state.lock().unwrap();
                state
                    .output
                    .extend_from_slice(marker.get(split..).expect("split is in the marker"));
                state.reader_done = true;
            }
            cancel.changed.notify_all();
            assert_eq!(
                Deadline::after(WITHIN)
                    .recv(&done)
                    .unwrap_or_else(|_| panic!(
                        "waited for the joined marker at prefix {prefix_len}"
                    )),
                Ok(Vec::new()),
                "prefix {prefix_len}"
            );
        } else {
            assert_eq!(
                Deadline::after(WITHIN)
                    .recv(&done)
                    .unwrap_or_else(|_| panic!(
                        "waited for the over-cap split at prefix {prefix_len}"
                    )),
                Err(Failed::TooLarge),
                "prefix {prefix_len}"
            );
        }
    }
}

/// Runs `read_all` over `written` (then end of file) starting from an
/// empty buffer, and returns what it retained and whether the reader
/// ended. The stream starts with the framed bytes when `written` does, so
/// a marker split across reads still seals exactly at its end whatever
/// the chunking.
fn read_all_with(written: Vec<u8>, nonce: &str) -> (Vec<u8>, bool) {
    let cancel = Cancel::new();
    let marker = end(nonce);
    let (hub_end, mut child_end) = UnixStream::pair().unwrap();
    let (reader, stop) = support::stoppable::reader(hub_end).unwrap();
    // The reader runs beside the write: megabytes never fit the socket
    // buffer, so writing them all before reading would block forever.
    let moved = Arc::clone(&cancel);
    let (ended_tx, ended_rx) = mpsc::channel();
    thread::spawn(move || {
        read_all(reader, &moved, &marker);
        ended_tx.send(()).unwrap_or(());
    });
    child_end.write_all(&written).unwrap();
    drop(child_end);
    Deadline::after(WITHIN)
        .recv(&ended_rx)
        .expect("read_all ends on end of file");
    drop(stop);
    let state = cancel.state.lock().unwrap();
    (state.output.clone(), state.reader_done)
}

#[test]
fn read_all_bounds_retained_output() {
    let nonce = "bound";
    let marker = end(nonce);
    let bound = OUTPUT_CAP + marker.len();
    // The framed bytes, then descendant chatter: only through the
    // marker's end is retained, whatever the chunking, so the copy the
    // success path parses never rides the chatter along.
    let mut stream = framed(nonce, b"", b"", b"");
    stream.extend_from_slice(&vec![b'y'; 200_000]);
    let (retained, done) = read_all_with(stream, nonce);
    assert_eq!(
        retained,
        framed(nonce, b"", b"", b""),
        "only through the marker is retained"
    );
    assert!(done, "the reader still ends");
    // Past the cap plus one marker length with no marker, nothing more
    // is retained: one chunk may overshoot the bound, no more.
    let (retained, _) = read_all_with(vec![b'x'; bound + 32_768], nonce);
    assert!(retained.len() > bound, "everything to the bound is kept");
    assert!(
        retained.len() <= bound + 8192,
        "nothing past one chunk over the bound is retained: {}",
        retained.len()
    );
    // At exactly the bound the buffer still grows: this pins the `+`
    // against a `-` mutant, whose smaller bound would seal here.
    let (retained, _) = read_all_with(vec![b'x'; bound], nonce);
    assert_eq!(retained.len(), bound, "the bound itself still retains");
    // Below every bound, bytes are retained as before.
    let (retained, done) = read_all_with(b"hello".to_vec(), nonce);
    assert_eq!(retained, b"hello");
    assert!(done, "the reader still ends");
}
