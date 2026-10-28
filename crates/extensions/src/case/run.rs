//! Driving session cases (`docs/testing.md`, "Testing an extension").

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::extension::Drive;
use contract::inbox::{Ack, Rejection};
use serde_json::{Map, Value};
use support::clock::{Manual, Sleep};
use support::group::GROUP_POLL;

use super::format::{CaseCommand, ClockAdvance, Input, Selector, SessionCase};

/// The event source that feeds case events to the driver.
type EventSource = dyn FnMut(Duration) -> Option<Result<Option<contract::Envelope>, String>> + Send;

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
pub struct CaseRun {
    name: String,
    expected: Vec<Value>,
    host: Arc<crate::HostScript>,
    clock: Arc<Manual>,
    advances: Vec<ClockAdvance>,
    until: Option<Selector>,
    command: Option<CaseCommand>,
    attended: bool,
    process_clock: Arc<dyn Clock>,
    waits: WaitBounds,
    verdict: Mutex<Option<Vec<String>>>,
}

impl CaseRun {
    /// Builds the host script and manual clock for a parsed session case.
    pub fn new(case: SessionCase, process_clock: Arc<dyn Clock>) -> Arc<Self> {
        Self::with_waits(case, process_clock, RUNNER_WAITS)
    }

    fn with_waits(
        case: SessionCase,
        process_clock: Arc<dyn Clock>,
        waits: WaitBounds,
    ) -> Arc<Self> {
        let name = case.name.clone().unwrap_or_default();
        let command = match &case.input {
            Input::Prompt(_) => None,
            Input::Command(command) => Some(command.clone()),
        };
        Arc::new(Self {
            name,
            expected: case.expect,
            host: crate::HostScript::new(case.host.http, case.host.exec, case.host.oauth),
            clock: Manual::with_sleep(Sleep::Waits),
            advances: case.clock,
            until: case.until,
            command,
            attended: case.attended,
            process_clock,
            waits,
            verdict: Mutex::new(None),
        })
    }

    /// Whether a person is attached to the case's session.
    pub fn attended(&self) -> bool {
        self.attended
    }

    /// The clock this case's session and extension hooks share.
    pub fn session_clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock) as Arc<dyn Clock>
    }

    /// Host-call replies supplied only to this case's extensions.
    pub fn host_script(&self) -> Arc<crate::HostScript> {
        Arc::clone(&self.host)
    }

    /// Starts the event driver before the loop writes its first event.
    /// The caller registers its event source before calling this, so no
    /// event the session writes afterwards is lost.
    pub fn start(
        self: &Arc<Self>,
        driver: Arc<dyn Drive>,
        events: Box<EventSource>,
        cancel: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<JoinHandle<()>, crate::Error> {
        let case = Arc::clone(self);
        thread::Builder::new()
            .name("fiber-case-driver".to_owned())
            .spawn(move || {
                let verdict = case.drive(driver, events, cancel);
                *support::lock(&case.verdict) = Some(verdict);
            })
            .map_err(crate::Error::CaseDriver)
    }

    /// Adds a failure when the driver could not start or return a verdict.
    pub fn record_failure(&self, reason: String) {
        let mut verdict = support::lock(&self.verdict);
        verdict.get_or_insert_with(Vec::new).push(reason);
    }

    /// The child's final case verdict, after its session has stopped.
    pub fn verdict(&self) -> Option<Vec<String>> {
        support::lock(&self.verdict).clone()
    }

    fn drive(
        &self,
        driver: Arc<dyn Drive>,
        mut event_source: Box<EventSource>,
        cancel: Arc<dyn Fn() + Send + Sync>,
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
        let mut released = false;
        let mut command = self.command.clone();

        if !self.apply_advances(None, &mut advance, &mut failures) {
            released |= self.close(&driver, &cancel, &mut failures);
        }
        while !reached_until && failures.is_empty() {
            let last_advance_done = advance >= self.advances.len();
            let Some(line) = self.next_event(
                &mut event_source,
                deadline,
                &mut failures,
                last_advance_done,
            ) else {
                released |= self.close(&driver, &cancel, &mut failures);
                break;
            };
            let durable = line.is_durable();
            if durable {
                events.push(serde_json::to_value(&line).unwrap_or(Value::Null));
            }
            let occurrence = seen.entry(line.kind.clone()).or_default();
            *occurrence = occurrence.saturating_add(1);
            seen_order.push(line.kind.clone());
            if line.kind == "extensions_loaded"
                && let Some(command) = command.take()
                && !self.send_command(&driver, &cancel, &command, &mut failures)
            {
                break;
            }
            if !self.apply_advances(Some((&line.kind, *occurrence)), &mut advance, &mut failures) {
                released |= self.close(&driver, &cancel, &mut failures);
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
                // The exit wait below runs on reached_until alone, so
                // whether close released a hang is unobservable here: the
                // return is dropped instead of folded into released.
                self.close(&driver, &cancel, &mut failures);
            }
        }

        if (reached_until || released) && !has_fiber_exited(&events) {
            let exit_deadline = self
                .process_clock
                .now()
                .checked_add(self.waits.close)
                .unwrap_or_else(|| self.process_clock.now());
            while let Some(line) =
                self.next_event(&mut event_source, exit_deadline, &mut failures, false)
            {
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
                cancel();
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
        events: &mut EventSource,
        deadline: Instant,
        failures: &mut Vec<String>,
        last_advance_done: bool,
    ) -> Option<contract::Envelope> {
        let remaining = deadline.saturating_duration_since(self.process_clock.now());
        // The whole wait in poll slices, counted, so a hang that matches
        // mid-wait ends it within one slice. The count bounds the wait in
        // real time: the process clock may be a frozen case clock in tests,
        // while the event source below always waits in real time. An iterator
        // bounds the wait even when a mutant breaks the count: a hand-stepped
        // counter can spin past the 120 s mutant timeout instead of failing.
        let polls = remaining
            .as_millis()
            .div_ceil(GROUP_POLL.as_millis())
            .max(1);
        for _ in 0..polls {
            // Past the last advance a matched hang with a later deadline can
            // never be released: only the runner's advances move the case
            // clock. End the wait so close can name and release it.
            if last_advance_done && !self.host.pending_hangs(self.clock.now()).is_empty() {
                return None;
            }
            match events(remaining.min(GROUP_POLL)) {
                Some(Ok(Some(line))) => return Some(line),
                Some(Ok(None)) => {
                    failures.push("the session log ended before the case completed".to_owned());
                    return None;
                }
                Some(Err(error)) => {
                    failures.push(format!("reading the session log: {error}"));
                    return None;
                }
                None => {}
            }
        }
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
            if let Some(hang) = entry.hang {
                // Hold the advance until the named entry matches as a hang,
                // so the advance lands on the hang's park instead of an
                // earlier wait on the same deadline.
                if let Err(reason) = self.host.wait_hang_matched(hang, self.waits.advance) {
                    failures.push(format!("clock advance[{index}]: {reason}"));
                    return false;
                }
            }
            let duration = Duration::from_millis(entry.advance_ms);
            if let Err(reason) = self
                .clock
                .advance_when_parked_in(duration, self.waits.advance)
            {
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

    /// Sends the case's extension command after `extensions_loaded`, as a
    /// person typing `/name text`. A rejection or an unanswered ack fails the
    /// case, then closes the session straight away so the session does not
    /// serve until the process is killed.
    fn send_command(
        &self,
        driver: &Arc<dyn Drive>,
        cancel: &Arc<dyn Fn() + Send + Sync>,
        command: &CaseCommand,
        failures: &mut Vec<String>,
    ) -> bool {
        let args = Map::from_iter([
            ("name".to_owned(), Value::String(command.name.clone())),
            ("text".to_owned(), Value::String(command.text.clone())),
        ]);
        match self.ask(driver, cancel, "command", args, self.waits.until) {
            Some(Ok(())) => true,
            Some(Err(rejection)) => {
                let code = serde_json::to_value(&rejection.code)
                    .ok()
                    .and_then(|code| code.as_str().map(str::to_owned))
                    .unwrap_or_else(|| format!("{:?}", rejection.code));
                failures.push(format!(
                    "command {}: {} ({code})",
                    command.name, rejection.message
                ));
                self.close(driver, cancel, failures);
                false
            }
            None => {
                failures.push(format!(
                    "command {}: no answer within {:?}",
                    command.name, self.waits.until
                ));
                self.close(driver, cancel, failures);
                false
            }
        }
    }

    /// Drives one command and waits for its acknowledgement. `None` is an
    /// unanswered ack; the caller names it and cancels.
    fn ask(
        &self,
        driver: &Arc<dyn Drive>,
        cancel: &Arc<dyn Fn() + Send + Sync>,
        command: &str,
        args: Map<String, Value>,
        wait: Duration,
    ) -> Option<Result<(), Rejection>> {
        let (answer, received) = mpsc::channel();
        driver.drive(
            "case-runner",
            command,
            args,
            Ack(Box::new(move |result| {
                drop(answer.send(result));
            })),
        );
        #[allow(
            clippy::disallowed_methods,
            reason = "the case runner's wait for the session's acknowledgement, bounded by the caller's wait"
        )]
        let answer = received.recv_timeout(wait);
        let answer = answer.ok().map(|result| result.map(|_| ()));
        if answer.is_none() {
            cancel();
        }
        answer
    }

    /// Closes the session, releasing stuck hangs first. Past the last clock
    /// entry a matched hang with a later deadline is stuck: this names each
    /// one, advances to its own deadline in deadline order so its callback
    /// ends at its timeout, then runs the plain close. Returns whether any
    /// hang was released.
    fn close(
        &self,
        driver: &Arc<dyn Drive>,
        cancel: &Arc<dyn Fn() + Send + Sync>,
        failures: &mut Vec<String>,
    ) -> bool {
        let mut stuck = self.host.pending_hangs(self.clock.now());
        stuck.sort_by_key(|stuck| stuck.2);
        let mut released = false;
        for (entry, request, deadline) in stuck {
            failures.push(format!(
                "unmet host call: host.exec[{entry}] hung: request {request} still parked after the last clock advance"
            ));
            released = true;
            // pending_hangs only holds deadlines later than now, so the
            // wait below is never zero on this path; saturating keeps it
            // total if the clock ever moves between the two reads.
            let wait = deadline.saturating_duration_since(self.clock.now());
            if wait.is_zero() {
                continue;
            }
            if let Err(reason) = self.clock.advance_when_parked_in(wait, self.waits.advance) {
                failures.push(format!("host.exec[{entry}]: {reason}"));
            }
        }
        match self.ask(driver, cancel, "close", Map::new(), self.waits.close) {
            Some(Ok(())) => {}
            Some(Err(rejection)) => failures.push(format!(
                "closing the case session: {} ({:?})",
                rejection.message, rejection.code
            )),
            None => {
                failures.push("closing the case session timed out".to_owned());
            }
        }
        released
    }
}

fn has_fiber_exited(events: &[Value]) -> bool {
    events
        .iter()
        .any(|event| event.get("kind").and_then(Value::as_str) == Some("fiber_exited"))
}

/// Checks durable event lines, in order, against a case's expected subsets.
fn session_verdict(expected: &[Value], events: &[Value], unmet: &[String]) -> Vec<String> {
    let durable: Vec<_> = events
        .iter()
        .filter(|event| event.get("seq").is_some_and(|seq| !seq.is_null()))
        .collect();
    let mut failures = Vec::new();
    for (index, (expected, actual)) in expected.iter().zip(&durable).enumerate() {
        if let Err(reason) = crate::json_matches(expected, actual) {
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

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
