//! The `call` jig (`docs/testing.md`, "Jigs"): runs one tool call and prints
//! its result as JSON.
//!
//! `cargo run -p tools --example call -- shell '{"command":"echo hi"}'`

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

use std::process::ExitCode;
use std::sync::{Arc, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use contract::clock::{Clock, Wake};
use contract::tool::{Output, Tool};
use fakes::CancelToken;
use serde_json::{Map, Value};
use tools::Shell;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(name) = args.next() else {
        eprintln!("usage: call shell '{{...}}'");
        return ExitCode::from(2);
    };
    let Some(raw) = args.next() else {
        eprintln!("usage: call shell '{{...}}'");
        return ExitCode::from(2);
    };
    if args.next().is_some() || name != "shell" {
        eprintln!("usage: call shell '{{...}}'");
        return ExitCode::from(2);
    }
    let arguments = match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(arguments)) => arguments,
        Ok(_) => {
            eprintln!("call: the arguments are not a JSON object");
            return ExitCode::from(2);
        }
        Err(err) => {
            eprintln!("call: {err}");
            return ExitCode::from(2);
        }
    };
    let workspace = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("call: {err}");
            return ExitCode::from(1);
        }
    };
    let output = Shell::new(workspace, Arc::new(ProcessClock)).run(&arguments, &CancelToken::new());
    match serde_json::to_string(&output_json(&output)) {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("call: {err}");
            ExitCode::from(1)
        }
    }
}

fn output_json(output: &Output) -> Map<String, Value> {
    let mut map = Map::new();
    if let Ok(content) = serde_json::to_value(&output.content) {
        map.insert("content".to_owned(), content);
    }
    if let Some(error) = &output.error
        && let Ok(error) = serde_json::to_value(error)
    {
        map.insert("error".to_owned(), error);
    }
    if let Some(process) = &output.process
        && let Ok(process) = serde_json::to_value(process)
    {
        map.insert("process".to_owned(), process);
    }
    if let Some(details) = &output.details {
        map.insert("details".to_owned(), details.clone());
    }
    if let Some(changes) = &output.changes
        && let Ok(changes) = serde_json::to_value(changes)
    {
        map.insert("changes".to_owned(), changes);
    }
    if let Some(control) = &output.control
        && let Ok(control) = serde_json::to_value(control)
    {
        map.insert("control".to_owned(), control);
    }
    map
}

/// The process clock. `tools` cannot depend on `main`, which owns the one the
/// binary uses, so the jig keeps a small one.
struct ProcessClock;

impl Clock for ProcessClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "the call jig reads the process clock; tools cannot depend on main"
    )]
    fn now(&self) -> Instant {
        Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the call jig reads the process clock; tools cannot depend on main"
    )]
    fn wall(&self) -> SystemTime {
        SystemTime::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the call jig waits on the process clock; tools cannot depend on main"
    )]
    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        let bound = until.map(|until| until.saturating_duration_since(self.now()));
        wait(bound);
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}
