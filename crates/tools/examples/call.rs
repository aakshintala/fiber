//! The `call` jig (`docs/testing.md`, "Jigs"): runs one tool call and prints
//! its result as JSON.
//!
//! `cargo run -p tools --example call -- shell '{"command":"echo hi"}'`
//!
//! `read`, `write` and `edit` use a fresh session, so a replacing `write` is
//! refused with `stale_file`. `web_fetch` saves its artifacts in a temporary
//! directory removed on exit, and measures its deadlines on the system clock.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

use std::process::ExitCode;
use std::sync::Arc;

use contract::tool::{Output, Tool};
use fakes::{CancelToken, Recorder, TempDir};
use serde_json::{Map, Value};
use tools::{Files, Shell, WebFetch};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let name = args.next();
    let raw = args.next();
    let extra = args.next();
    let Some(name) = name.filter(|_| extra.is_none()) else {
        usage();
        return ExitCode::from(2);
    };
    let Some(raw) = raw else {
        usage();
        return ExitCode::from(2);
    };
    if !matches!(
        name.as_str(),
        "read" | "write" | "edit" | "shell" | "web_fetch"
    ) {
        usage();
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
    let cancel = CancelToken::new();
    let output = match name.as_str() {
        "shell" => Shell::new(workspace, Arc::new(fakes::clock::SystemClock)).run(
            &arguments,
            &cancel,
            &Recorder::default(),
        ),
        "read" => Files::new(workspace)
            .read()
            .run(&arguments, &cancel, &Recorder::default()),
        "write" => Files::new(workspace)
            .write()
            .run(&arguments, &cancel, &Recorder::default()),
        "edit" => Files::new(workspace)
            .edit()
            .run(&arguments, &cancel, &Recorder::default()),
        "web_fetch" => {
            let session = TempDir::new("fiber-call-web-fetch");
            WebFetch::new(
                session.path().join("artifacts"),
                Arc::new(fakes::clock::SystemClock),
            )
            .run(
                &arguments,
                &cancel,
                &Recorder::default(),
            )
        }
        _ => {
            usage();
            return ExitCode::from(2);
        }
    };
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

fn usage() {
    eprintln!("usage: call <read|write|edit|shell|web_fetch> '{{...}}'");
    eprintln!("A replacing write is refused with stale_file: each run is a fresh session.");
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

