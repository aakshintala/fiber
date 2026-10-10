//! Driving session cases (`docs/testing.md`, "Testing an extension").

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::extension::Drive;
use contract::inbox::Ack;
use log::{Log, Watcher};
use serde_json::{Map, Value};

use super::clock::CaseClock;
use super::format::{Case, ClockAdvance, Host, Selector, SessionCase};

pub(super) const ADVANCE_WAIT: Duration = Duration::from_secs(10);
pub(super) const UNTIL_WAIT: Duration = Duration::from_secs(30);
const CLOSE_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
struct WaitBounds {
    advance: Duration,
    until: Duration,
    close: Duration,
}

const RUNNER_WAITS: WaitBounds = WaitBounds {
    advance: ADVANCE_WAIT,
    until: UNTIL_WAIT,
    close: CLOSE_WAIT,
};

/// Inputs and results for one runner-only session case.
pub(crate) struct CaseRun {
    name: String,
    expected: Vec<Value>,
    host: Arc<extensions::HostScript>,
    clock: Arc<CaseClock>,
    advances: Vec<ClockAdvance>,
    until: Option<Selector>,
    process_clock: Arc<dyn Clock>,
    waits: WaitBounds,
    verdict: Mutex<Option<Vec<String>>>,
}

impl CaseRun {
    /// Builds the host script and manual clock for a parsed session case.
    pub(crate) fn new(
        name: String,
        expected: Vec<Value>,
        host: Host,
        advances: Vec<ClockAdvance>,
        until: Option<Selector>,
        process_clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        Self::with_waits(
            name,
            expected,
            host,
            advances,
            until,
            process_clock,
            RUNNER_WAITS,
        )
    }

    fn with_waits(
        name: String,
        expected: Vec<Value>,
        host: Host,
        advances: Vec<ClockAdvance>,
        until: Option<Selector>,
        process_clock: Arc<dyn Clock>,
        waits: WaitBounds,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            expected,
            host: extensions::HostScript::new(host.http, host.exec, host.oauth),
            clock: CaseClock::new(),
            advances,
            until,
            process_clock,
            waits,
            verdict: Mutex::new(None),
        })
    }

    /// The clock this case's session and extension hooks share.
    pub(crate) fn session_clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock) as Arc<dyn Clock>
    }

    /// Host-call replies supplied only to this case's extensions.
    pub(crate) fn host_script(&self) -> Arc<extensions::HostScript> {
        Arc::clone(&self.host)
    }

    /// Starts the event driver before the loop writes its first event.
    /// The event watcher is registered before this returns, so no event
    /// the session writes afterwards is lost.
    pub(crate) fn start(
        self: &Arc<Self>,
        driver: Arc<dyn Drive>,
        log: Arc<Log>,
        cancel: Arc<r#loop::TurnCancel>,
    ) -> std::io::Result<JoinHandle<()>> {
        let watcher = log.watch_all();
        let case = Arc::clone(self);
        thread::Builder::new()
            .name("fiber-case-driver".to_owned())
            .spawn(move || {
                let verdict = case.drive(driver, watcher, cancel);
                *lock(&case.verdict) = Some(verdict);
            })
    }

    /// Adds a failure when the driver could not start or return a verdict.
    pub(crate) fn record_failure(&self, reason: String) {
        let mut verdict = lock(&self.verdict);
        verdict.get_or_insert_with(Vec::new).push(reason);
    }

    /// The child's final case verdict, after its session has stopped.
    pub(crate) fn verdict(&self) -> Option<Vec<String>> {
        lock(&self.verdict).clone()
    }

    fn drive(
        &self,
        driver: Arc<dyn Drive>,
        mut watcher: Watcher,
        cancel: Arc<r#loop::TurnCancel>,
    ) -> Vec<String> {
        let deadline = self
            .process_clock
            .now()
            .checked_add(self.waits.until)
            .unwrap_or_else(|| self.process_clock.now());
        let mut events = Vec::new();
        let mut failures = Vec::new();
        let mut seen = BTreeMap::<String, usize>::new();
        let mut seen_order = Vec::<String>::new();
        let mut advance = 0;
        let mut reached_until = false;

        if !self.apply_advances(None, &mut advance, &mut failures) {
            self.close(&driver, &cancel, &mut failures);
        }
        while !reached_until && failures.is_empty() {
            let Some(line) = self.next_event(&mut watcher, deadline, &mut failures) else {
                self.close(&driver, &cancel, &mut failures);
                break;
            };
            let durable = line.is_durable();
            if durable {
                events.push(serde_json::to_value(&line).unwrap_or(Value::Null));
            }
            let occurrence = seen.entry(line.kind.clone()).or_default();
            *occurrence = occurrence.saturating_add(1);
            seen_order.push(line.kind.clone());
            if !self.apply_advances(Some((&line.kind, *occurrence)), &mut advance, &mut failures) {
                self.close(&driver, &cancel, &mut failures);
                break;
            }
            reached_until = self.is_until(&line.kind, *occurrence);
            if line.kind == "fiber_exited" {
                if !reached_until {
                    failures.push("the session exited before its until event".to_owned());
                }
                break;
            }
            if reached_until {
                self.close(&driver, &cancel, &mut failures);
            }
        }

        if reached_until && !has_fiber_exited(&events) {
            let exit_deadline = self
                .process_clock
                .now()
                .checked_add(self.waits.close)
                .unwrap_or_else(|| self.process_clock.now());
            while let Some(line) = self.next_event(&mut watcher, exit_deadline, &mut failures) {
                seen_order.push(line.kind.clone());
                if line.is_durable() {
                    events.push(serde_json::to_value(&line).unwrap_or(Value::Null));
                }
                if line.kind == "fiber_exited" {
                    break;
                }
            }
            if !has_fiber_exited(&events) {
                failures.push("the session did not write fiber_exited after close".to_owned());
                cancel.cancel();
            }
        }
        if advance < self.advances.len() {
            let next = advance.saturating_add(1);
            failures.push(format!("clock advance[{next}] was not reached"));
        }
        failures.extend(session_verdict(&self.expected, &events, &self.host.unmet()));
        if !reached_until || !has_fiber_exited(&events) || advance < self.advances.len() {
            failures.push(self.diagnostics(advance, &seen_order));
        }
        failures
    }

    /// The failure dump names the clock's parked deadlines, the event
    /// kinds seen in order and the next unapplied advance.
    fn diagnostics(&self, next: usize, seen: &[String]) -> String {
        let (offset, parked) = self.clock.parked_offsets();
        let parked = parked
            .iter()
            .map(|deadline| match deadline {
                Some(deadline) => format!("{}ms", deadline.as_millis()),
                None => "none".to_owned(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let next = if next < self.advances.len() {
            next.saturating_add(1).to_string()
        } else {
            "none".to_owned()
        };
        format!(
            "diagnostics: now_offset_ms={} parked_ms=[{parked}] events=[{}] next_advance={next}",
            offset.as_millis(),
            seen.join(", ")
        )
    }

    fn next_event(
        &self,
        watcher: &mut Watcher,
        deadline: Instant,
        failures: &mut Vec<String>,
    ) -> Option<contract::Envelope> {
        let remaining = deadline.saturating_duration_since(self.process_clock.now());
        match watcher.recv_timeout(remaining) {
            Some(Ok(Some(line))) => Some(line),
            Some(Ok(None)) => {
                failures.push("the session log ended before the case completed".to_owned());
                None
            }
            Some(Err(error)) => {
                failures.push(format!("reading the session log: {error}"));
                None
            }
            None => {
                let until = self
                    .until
                    .as_ref()
                    .map(|selector| format!("{} occurrence {}", selector.kind, selector.nth))
                    .unwrap_or_else(|| "turn_completed or turn_failed".to_owned());
                failures.push(format!(
                    "{}: wait for until event {until} expired after {:?}",
                    self.name, self.waits.until
                ));
                None
            }
        }
    }

    fn apply_advances(
        &self,
        current: Option<(&str, usize)>,
        next: &mut usize,
        failures: &mut Vec<String>,
    ) -> bool {
        while let Some(entry) = self.advances.get(*next) {
            let ready = match (&entry.after, current) {
                (None, _) => true,
                (Some(selector), Some((kind, occurrence))) => {
                    selector.kind == kind && selector.nth == occurrence
                }
                (Some(_), None) => false,
            };
            if !ready {
                break;
            }
            let index = next.saturating_add(1);
            let duration = Duration::from_millis(entry.advance_ms);
            if let Err(reason) = self.clock.advance_when_parked(duration, self.waits.advance) {
                failures.push(format!("clock advance[{index}]: {reason}"));
                return false;
            }
            *next = next.saturating_add(1);
        }
        true
    }

    fn is_until(&self, kind: &str, occurrence: usize) -> bool {
        match &self.until {
            Some(selector) => selector.kind == kind && selector.nth == occurrence,
            None => occurrence == 1 && matches!(kind, "turn_completed" | "turn_failed"),
        }
    }

    fn close(
        &self,
        driver: &Arc<dyn Drive>,
        cancel: &Arc<r#loop::TurnCancel>,
        failures: &mut Vec<String>,
    ) {
        let (answer, received) = mpsc::channel();
        driver.drive(
            "case-runner",
            "close",
            Map::new(),
            Ack(Box::new(move |result| {
                drop(answer.send(result));
            })),
        );
        match received.recv_timeout(self.waits.close) {
            Ok(Ok(_)) => {}
            Ok(Err(rejection)) => failures.push(format!(
                "closing the case session: {} ({:?})",
                rejection.message, rejection.code
            )),
            Err(_) => {
                failures.push("closing the case session timed out".to_owned());
                cancel.cancel();
            }
        }
    }
}

/// Checks durable event lines, in order, against a case's expected subsets.
pub(crate) fn session_verdict(
    expected: &[Value],
    events: &[Value],
    unmet: &[String],
) -> Vec<String> {
    let durable: Vec<_> = events
        .iter()
        .filter(|event| event.get("seq").is_some_and(|seq| !seq.is_null()))
        .collect();
    let mut failures = Vec::new();
    for (index, (expected, actual)) in expected.iter().zip(&durable).enumerate() {
        if let Err(reason) = extensions::json_matches(expected, actual) {
            failures.push(format!("expect[{index}].{reason}"));
            break;
        }
    }
    if expected.len() > durable.len() {
        failures.push(format!("expect[{}]: missing durable event", durable.len()));
    } else if durable.len() > expected.len() {
        let index = expected.len();
        let kind = durable
            .get(index)
            .and_then(|event| event.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        failures.push(format!("event[{index}]: unexpected durable {kind}"));
    }
    failures.extend(
        unmet
            .iter()
            .map(|entry| format!("unmet host call: {entry}")),
    );
    failures
}

/// Runs one case file and prints its child verdict.
pub(crate) fn extension_case(
    path: PathBuf,
    process_clock: Arc<dyn Clock>,
    fiber: Result<PathBuf, String>,
) -> i32 {
    let fallback_name = path
        .file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return malformed(&fallback_name, &[format!("{}: {error}", path.display())]);
        }
    };
    let case = match Case::parse(&path, &bytes) {
        Ok(case) => case,
        Err(error) => return malformed(&fallback_name, &[error]),
    };
    let (name, case) = match case {
        Case::Call(call) => {
            let name = call.name.clone().unwrap_or(fallback_name);
            return super::call::run_case(call, name, process_clock);
        }
        Case::Session(session) => (session.name.clone().unwrap_or(fallback_name), session),
    };
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(error) => return report(&name, &[format!("the case workspace: {error}")]),
    };
    if let Err(error) = write_inputs(&case, &workspace) {
        return report(&name, &[error]);
    }
    let case_run = CaseRun::new(
        name.clone(),
        case.expect,
        case.host,
        case.clock,
        case.until,
        Arc::clone(&process_clock),
    );
    let signals = match doors::Signals::install(process_clock) {
        Ok(signals) => signals,
        Err(error) => return report(&name, &[format!("case signals: {error}")]),
    };
    let result = crate::session_command::new_session(
        contract::SessionId(doors::mint("s_")),
        crate::per_run(Some("scripted/script.json".to_owned()), Vec::new()),
        Some(case.prompt),
        false,
        false,
        case_run.session_clock(),
        &signals,
        fiber,
        Some(Arc::clone(&case_run)),
        None,
    );
    let failures = case_run
        .verdict()
        .unwrap_or_else(|| vec![format!("session setup failed with exit code {result}")]);
    report(&name, &failures)
}

fn write_inputs(case: &SessionCase, workspace: &Path) -> Result<(), String> {
    let script_path = workspace.join("script.json");
    let script_bytes = serde_json::to_vec(&case.script)
        .map_err(|error| format!("script: cannot encode: {error}"))?;
    fs::write(&script_path, script_bytes)
        .map_err(|error| format!("{}: {error}", script_path.display()))?;
    if let Some(config) = &case.config {
        let home = config::fiber_home_from_env().map_err(|error| error.to_string())?;
        let config_path = home.join("config.json");
        let config_bytes = serde_json::to_vec(config)
            .map_err(|error| format!("config: cannot encode: {error}"))?;
        fs::write(&config_path, config_bytes)
            .map_err(|error| format!("{}: {error}", config_path.display()))?;
    }
    Ok(())
}

fn has_fiber_exited(events: &[Value]) -> bool {
    events
        .iter()
        .any(|event| event.get("kind").and_then(Value::as_str) == Some("fiber_exited"))
}

pub(super) fn report(name: &str, failures: &[String]) -> i32 {
    verdict(name, failures, 1)
}

/// A malformed case: invalid JSON, an unsupported function or a bad field.
/// The case never ran, so this is a wrong invocation, which exits 2
/// (`docs/invocation.md`, "Commands and flags").
pub(super) fn malformed(name: &str, failures: &[String]) -> i32 {
    verdict(name, failures, 2)
}

#[allow(
    clippy::print_stdout,
    reason = "the hidden child reports one verdict on stdout (`docs/testing.md`, \"Testing an extension\")"
)]
fn verdict(name: &str, failures: &[String], code: i32) -> i32 {
    if failures.is_empty() {
        println!("ok {name}");
        return 0;
    }
    println!("FAIL {name}");
    for failure in failures {
        println!("  {failure}");
    }
    code
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
