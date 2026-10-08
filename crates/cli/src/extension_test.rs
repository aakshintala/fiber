//! `fiber extension test` (`docs/testing.md`, "Testing an extension"): install
//! one package into a fresh Fiber home for each case, run the hidden case child
//! in its own process group, and print one summary.

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};
use extensions::{Origin, Request};
use rustix::process::{Pid, Signal};

const CASE_DEADLINE: Duration = Duration::from_secs(60);
const SECOND_SIGTERM_GAP: Duration = Duration::from_secs(1);
const TERM_GRACE: Duration = Duration::from_secs(5);
const REAP_DEADLINE: Duration = Duration::from_secs(5);

/// The parent process environment passed to each case child.
#[derive(Clone)]
struct ChildEnvironment {
    path: Option<OsString>,
    home: Option<OsString>,
}

impl ChildEnvironment {
    fn from_process() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            home: std::env::var_os("HOME"),
        }
    }
}

/// Time bounds are injected so process handling can be tested without the
/// process clock (`docs/testing.md`, "Waits and timeouts").
#[derive(Clone, Copy)]
struct RunTimeouts {
    case: Duration,
    second_sigterm: Duration,
    term_grace: Duration,
    reap: Duration,
}

impl RunTimeouts {
    fn process() -> Self {
        Self {
            case: CASE_DEADLINE,
            second_sigterm: SECOND_SIGTERM_GAP,
            term_grace: TERM_GRACE,
            reap: REAP_DEADLINE,
        }
    }
}

/// Dependencies for one runner invocation. Tests supply the clock,
/// environment and child command prefix instead of mutating process state.
struct RunOptions {
    clock: Arc<dyn Clock>,
    timeouts: RunTimeouts,
    environment: ChildEnvironment,
    temp_root: PathBuf,
    fiber_prefix: Vec<OsString>,
}

/// `fiber extension test [<path>]`: run every case in a package directory.
pub fn extension_test(path: Option<&Path>, fiber: Result<PathBuf, String>) -> i32 {
    let fiber = match fiber {
        Ok(fiber) => fiber,
        Err(error) => {
            writeln!(io::stderr().lock(), "fiber: {error}").unwrap_or(());
            return 1;
        }
    };
    let options = RunOptions {
        clock: Arc::new(ProcessClock),
        timeouts: RunTimeouts::process(),
        environment: ChildEnvironment::from_process(),
        temp_root: std::env::temp_dir(),
        fiber_prefix: Vec::new(),
    };
    extension_test_with(
        path,
        &fiber,
        &options,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

fn extension_test_with(
    path: Option<&Path>,
    fiber: &Path,
    options: &RunOptions,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let typed_path = path.unwrap_or_else(|| Path::new("."));
    let package = match typed_path.canonicalize() {
        Ok(package) if package.join("extension.json").is_file() => package,
        Ok(_) | Err(_) => {
            writeln!(
                err,
                "fiber: {} is not an extension package (missing extension.json)",
                typed_path.display()
            )
            .unwrap_or(());
            return 2;
        }
    };
    let cases = match discover_cases(&package) {
        Ok(cases) => cases,
        Err(error) => {
            writeln!(err, "fiber: {error}").unwrap_or(());
            return 1;
        }
    };
    if cases.is_empty() {
        writeln!(out, "no cases in {}/tests", package.display()).unwrap_or(());
        return 1;
    }

    let mut passed = 0;
    let mut failed = 0;
    for case in cases {
        let fallback_name = case_name(&case);
        let outcome = run_case(&package, &case, &fallback_name, fiber, options);
        if outcome.passed {
            passed += 1;
            writeln!(out, "ok {}", outcome.name).unwrap_or(());
        } else {
            failed += 1;
            writeln!(out, "FAIL {}", outcome.name).unwrap_or(());
            for reason in outcome.reasons {
                writeln!(out, "  {}", reason.trim()).unwrap_or(());
            }
        }
    }
    write!(out, "{}", summary(passed, failed)).unwrap_or(());
    if failed == 0 { 0 } else { 1 }
}

/// The direct case files, ordered by their filename bytes.
fn discover_cases(package: &Path) -> io::Result<Vec<PathBuf>> {
    let tests = package.join("tests");
    let entries = match fs::read_dir(&tests) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut cases = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.path().extension() == Some(OsStr::new("json")) {
            cases.push(entry.path());
        }
    }
    cases.sort_by(|left, right| {
        left.file_name()
            .unwrap_or_default()
            .as_encoded_bytes()
            .cmp(right.file_name().unwrap_or_default().as_encoded_bytes())
    });
    Ok(cases)
}

struct CaseOutcome {
    name: String,
    passed: bool,
    reasons: Vec<String>,
}

fn case_name(path: &Path) -> String {
    path.file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn run_case(
    package: &Path,
    case: &Path,
    fallback_name: &str,
    fiber: &Path,
    options: &RunOptions,
) -> CaseOutcome {
    let outcome = |reason: String| CaseOutcome {
        name: fallback_name.to_owned(),
        passed: false,
        reasons: vec![reason],
    };
    let directory = match RunDirectory::new(&options.temp_root) {
        Ok(directory) => directory,
        Err(error) => return outcome(format!("temporary case directory: {error}")),
    };
    let mut result = (|| {
        let home = directory.path.join("home");
        let workspace = directory.path.join("workspace");
        if let Err(error) = fs::create_dir_all(&home).and_then(|()| fs::create_dir_all(&workspace))
        {
            return outcome(format!("temporary case files: {error}"));
        }
        let plan = match extensions::plan(
            &home,
            &Request::Path(package.to_path_buf()),
            env!("CARGO_PKG_VERSION"),
            &Origin::github(),
            options.clock.as_ref(),
        ) {
            Ok(plan) => plan,
            Err(error) => return outcome(format!("installing the package: {error}")),
        };
        if let Err(error) = plan.commit() {
            return outcome(format!("installing the package: {error}"));
        }
        let stdout = directory.path.join("child.stdout");
        let output = match run_child(fiber, case, &home, &workspace, &stdout, options) {
            Ok(output) => output,
            Err(error) => return outcome(error),
        };
        match output {
            ChildOutcome::TimedOut { text } => {
                let lines: Vec<_> = text.lines().collect();
                let (name, _) = parse_verdict(&lines, fallback_name);
                CaseOutcome {
                    name,
                    passed: false,
                    reasons: vec![format!("did not finish within {:?}", options.timeouts.case)],
                }
            }
            ChildOutcome::Exited { status, text } => {
                let lines: Vec<&str> = text.lines().collect();
                let (name, reasons) = parse_verdict(&lines, fallback_name);
                let passed = status.success()
                    && reasons.is_empty()
                    && lines.first().is_some_and(|line| line.starts_with("ok "));
                if passed {
                    CaseOutcome {
                        name,
                        passed: true,
                        reasons: Vec::new(),
                    }
                } else {
                    let reasons = if reasons.is_empty() {
                        vec![format!(
                            "case child exited with {} without a successful verdict",
                            status_text(status)
                        )]
                    } else {
                        reasons
                    };
                    CaseOutcome {
                        name,
                        passed: false,
                        reasons,
                    }
                }
            }
        }
    })();
    if let Err(error) = directory.cleanup() {
        result.passed = false;
        result
            .reasons
            .push(format!("removing temporary case files: {error}"));
    }
    result
}

fn parse_verdict(lines: &[&str], fallback_name: &str) -> (String, Vec<String>) {
    let Some(first) = lines.first() else {
        return (
            fallback_name.to_owned(),
            vec!["case child wrote no verdict".to_owned()],
        );
    };
    let name = first
        .strip_prefix("ok ")
        .or_else(|| first.strip_prefix("FAIL "))
        .unwrap_or(fallback_name)
        .to_owned();
    let reasons = lines
        .iter()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_owned())
        .collect();
    (name, reasons)
}

fn status_text(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "a signal".to_owned(),
    }
}

fn summary(passed: usize, failed: usize) -> String {
    format!("{passed} passed, {failed} failed\n")
}

fn child_command(
    fiber: &Path,
    case: &Path,
    home: &Path,
    workspace: &Path,
    environment: &ChildEnvironment,
    prefix: &[OsString],
) -> Command {
    let mut command = Command::new(fiber);
    command
        .args(prefix)
        .arg("extension-case")
        .arg(case)
        .current_dir(workspace)
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(path) = &environment.path {
        command.env("PATH", path);
    }
    if let Some(home) = &environment.home {
        command.env("HOME", home);
    }
    command.env("FIBER_HOME", home);
    command
}

enum ChildOutcome {
    Exited { status: ExitStatus, text: String },
    TimedOut { text: String },
}

fn run_child(
    fiber: &Path,
    case: &Path,
    home: &Path,
    workspace: &Path,
    stdout: &Path,
    options: &RunOptions,
) -> Result<ChildOutcome, String> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(stdout)
        .map_err(|error| format!("child stdout file: {error}"))?;
    let mut command = child_command(
        fiber,
        case,
        home,
        workspace,
        &options.environment,
        &options.fiber_prefix,
    );
    command.stdout(Stdio::from(file));
    let child = command
        .spawn()
        .map_err(|error| format!("starting case child: {error}"))?;
    let group = child.id();

    let signal = Arc::new(WaitSignal::default());
    let clock_wake: Arc<dyn Wake> = signal.clone();
    options.clock.subscribe(Arc::downgrade(&clock_wake));
    let (send, receive) = mpsc::channel();
    let reaper_child = Arc::new(Mutex::new(Some(child)));
    let child_for_reaper = Arc::clone(&reaper_child);
    let wake = Arc::clone(&signal);
    let reaper = thread::Builder::new()
        .name("fiber-extension-case-reap".to_owned())
        .spawn(move || {
            let result = lock(&child_for_reaper)
                .take()
                .map(wait_child)
                .unwrap_or_else(|| Err(io::Error::other("case child handle was already taken")));
            let _sent = send.send(result);
            wake.wake();
        });
    if let Err(error) = reaper {
        let Some(mut child) = lock(&reaper_child).take() else {
            return Err(format!("starting case child reaper: {error}"));
        };
        let kill_error = signal_group(group, Signal::KILL).err();
        if kill_error.is_some() {
            let _killed = child.kill();
        }
        let reap_error = child.wait().err();
        let cleanup = [
            kill_error.map(|error| format!("SIGKILL: {error}")),
            reap_error.map(|error| format!("reaping: {error}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        return Err(if cleanup.is_empty() {
            format!("starting case child reaper: {error}")
        } else {
            format!(
                "starting case child reaper: {error}; cleanup failed: {}",
                cleanup.join(", ")
            )
        });
    }

    let deadline = after(options.clock.as_ref(), options.timeouts.case);
    if let Some(status) = wait_status(&receive, options.clock.as_ref(), deadline, signal.as_ref())?
    {
        let text = read_output(stdout)?;
        return Ok(ChildOutcome::Exited { status, text });
    }

    let started_term = options.clock.now();
    signal_group(group, Signal::TERM)
        .map_err(|error| format!("sending SIGTERM to case group: {error}"))?;
    let second_term = after(options.clock.as_ref(), options.timeouts.second_sigterm);
    wait_until(options.clock.as_ref(), second_term, signal.as_ref());
    signal_group(group, Signal::TERM)
        .map_err(|error| format!("sending the second SIGTERM to case group: {error}"))?;

    let grace_deadline = started_term
        .checked_add(options.timeouts.term_grace)
        .unwrap_or(started_term);
    wait_until(options.clock.as_ref(), grace_deadline, signal.as_ref());
    let mut status = wait_status(
        &receive,
        options.clock.as_ref(),
        grace_deadline,
        signal.as_ref(),
    )?;
    let group_remains =
        group_alive(group).map_err(|error| format!("checking the case process group: {error}"))?;
    if should_kill_group(status.is_none(), group_remains) {
        signal_group(group, Signal::KILL)
            .map_err(|error| format!("sending SIGKILL to case group: {error}"))?;
    }

    let reap_deadline = after(options.clock.as_ref(), options.timeouts.reap);
    if status.is_none() {
        status = wait_status(
            &receive,
            options.clock.as_ref(),
            reap_deadline,
            signal.as_ref(),
        )?;
    }
    if status.is_none() {
        return Err("case child was not reaped after SIGKILL".to_owned());
    }
    Ok(ChildOutcome::TimedOut {
        text: read_output(stdout)?,
    })
}

fn wait_child(mut child: Child) -> io::Result<ExitStatus> {
    child.wait()
}

fn read_output(path: &Path) -> Result<String, String> {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|error| format!("reading case child output: {error}"))
}

fn wait_status(
    receive: &mpsc::Receiver<io::Result<ExitStatus>>,
    clock: &dyn Clock,
    deadline: Instant,
    signal: &WaitSignal,
) -> Result<Option<ExitStatus>, String> {
    loop {
        signal.prepare_wait();
        match receive.try_recv() {
            Ok(Ok(status)) => return Ok(Some(status)),
            Ok(Err(error)) => return Err(format!("reaping case child: {error}")),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("case child reaper stopped without a result".to_owned());
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if clock.now() >= deadline {
            return Ok(None);
        }
        wait_once(clock, deadline, signal);
    }
}

fn wait_until(clock: &dyn Clock, deadline: Instant, signal: &WaitSignal) {
    loop {
        signal.prepare_wait();
        if clock.now() >= deadline {
            return;
        }
        wait_once(clock, deadline, signal);
    }
}

fn wait_once(clock: &dyn Clock, deadline: Instant, signal: &WaitSignal) {
    let mut wait = |timeout: Option<Duration>| {
        let guard = lock(&signal.notified);
        if *guard {
            return;
        }
        match timeout {
            Some(timeout) => {
                drop(
                    signal
                        .changed
                        .wait_timeout(guard, timeout)
                        .unwrap_or_else(PoisonError::into_inner),
                );
            }
            None => {
                drop(
                    signal
                        .changed
                        .wait(guard)
                        .unwrap_or_else(PoisonError::into_inner),
                );
            }
        }
    };
    clock.wait_until(Some(deadline), &mut wait);
}

fn after(clock: &dyn Clock, duration: Duration) -> Instant {
    clock
        .now()
        .checked_add(duration)
        .unwrap_or_else(|| clock.now())
}

fn signal_group(group: u32, signal: Signal) -> io::Result<()> {
    let id = process_group_id(group)?;
    match rustix::process::kill_process_group(id, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(io::Error::from_raw_os_error(error.raw_os_error())),
    }
}

fn group_alive(group: u32) -> io::Result<bool> {
    let id = process_group_id(group)?;
    Ok(rustix::process::test_kill_process_group(id).is_ok())
}

/// Refuses a process-group id that could reach processes outside this case
/// (`docs/testing.md`, "Running tests").
fn should_kill_group(status_missing: bool, group_remains: bool) -> bool {
    status_missing || group_remains
}

fn process_group_id(group: u32) -> io::Result<Pid> {
    if group <= 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to signal process group {group}"),
        ));
    }
    i32::try_from(group)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "process group id overflow"))
}

#[derive(Default)]
struct WaitSignal {
    notified: Mutex<bool>,
    changed: Condvar,
}

impl WaitSignal {
    fn prepare_wait(&self) {
        *lock(&self.notified) = false;
    }
}

impl Wake for WaitSignal {
    fn wake(&self) {
        *lock(&self.notified) = true;
        self.changed.notify_all();
    }
}

struct RunDirectory {
    path: PathBuf,
}

impl RunDirectory {
    fn new(parent: &Path) -> io::Result<Self> {
        fs::create_dir_all(parent)?;
        let path = parent.join(format!(
            "fiber-extension-test-{}-{}",
            std::process::id(),
            doors::mint("")
        ));
        fs::create_dir(&path)?;
        // Fiber home is private machine state (`docs/state.md`, "Override").
        if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o700)) {
            let _removed = fs::remove_dir_all(&path);
            return Err(error);
        }
        Ok(Self { path })
    }

    fn cleanup(&self) -> io::Result<()> {
        fs::remove_dir_all(&self.path)
    }
}

impl Drop for RunDirectory {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.path);
    }
}

/// The injected source of the process clock used for child deadlines.
struct ProcessClock;

impl Clock for ProcessClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind extension-case deadlines (`docs/testing.md`, \"Waits and timeouts\")"
    )]
    fn now(&self) -> Instant {
        Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind extension-case timestamps (`docs/testing.md`, \"Waits and timeouts\")"
    )]
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind extension-case waits (`docs/testing.md`, \"Waits and timeouts\")"
    )]
    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        wait(until.map(|until| until.saturating_duration_since(self.now())));
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn Wake>) {}
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "extension_test_tests.rs"]
mod tests;
