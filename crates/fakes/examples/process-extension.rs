//! The `process-extension` fixture (`docs/testing/fakes.md`): a checked-in
//! Rust program speaking the process extension wire over stdin and stdout.
//!
//! `cargo run -p fakes --example process-extension -- <mode> [config]` accepts
//! these modes:
//!
//! - `echo`: writes each stdin line back verbatim until EOF.
//! - `hold`: reads one line, acknowledges the hold, then waits for EOF and
//!   acknowledges release.
//! - `peer`: runs a bounded test protocol from one JSON action list. Actions
//!   read and write process-wire lines, hold for an acknowledged release,
//!   record a marker, or check a bounded silent interval. The parent test
//!   owns the rest of the deadline and process cleanup.
//!
//! Every stdout line is one JSON line, flushed at once. Diagnostics go to
//! stderr, never stdout. Any other mode exits 2.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a fixture's output is its interface"
)]

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsFd;
use std::process::{Command, ExitCode, Stdio};

use serde_json::{Map, Value};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_default();
    match mode.as_str() {
        "echo" => echo(),
        "hold" => hold(),
        "peer" => match args.next() {
            Some(config) => peer(&config),
            None => usage_error(),
        },
        _ => usage_error(),
    }
}

fn usage_error() -> ExitCode {
    eprintln!("usage: process-extension <echo|hold|peer> [json-config]");
    ExitCode::from(2)
}

/// Writes each stdin line back verbatim (flushed), until EOF.
fn echo() -> ExitCode {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in BufReader::new(stdin.lock()).lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                eprintln!("process-extension: stdin: {err}");
                return ExitCode::FAILURE;
            }
        };
        if out.write_all(line.as_bytes()).is_err() || out.write_all(b"\n").is_err() {
            return ExitCode::FAILURE;
        }
        if out.flush().is_err() {
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// Reads one line, acknowledges the hold, and waits for EOF before release.
fn hold() -> ExitCode {
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let mut first = String::new();
    match input.read_line(&mut first) {
        Ok(0) => {
            eprintln!("process-extension: hold ended before its line");
            return ExitCode::from(2);
        }
        Ok(_) => {}
        Err(err) => {
            eprintln!("process-extension: stdin: {err}");
            return ExitCode::FAILURE;
        }
    }
    let mut out = std::io::stdout().lock();
    if out.write_all(b"{\"held\":true}\n").is_err() || out.flush().is_err() {
        return ExitCode::FAILURE;
    }
    for line in input.lines() {
        if let Err(err) = line {
            eprintln!("process-extension: stdin: {err}");
            return ExitCode::FAILURE;
        }
    }
    if out.write_all(b"{\"released\":true}\n").is_err() || out.flush().is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Eof,
}

type Variables = BTreeMap<String, Value>;
type PeerInput<'a> = BufReader<std::io::StdinLock<'a>>;

/// Runs a JSON list of peer actions. The process owner remains responsible
/// for bounding every read and reaping this process group.
fn peer(config: &str) -> ExitCode {
    let actions: Value = match serde_json::from_str(config) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("process-extension: invalid peer config: {err}");
            return ExitCode::from(2);
        }
    };
    let term_marker = actions
        .as_object()
        .and_then(|config| config.get("term_marker"))
        .and_then(Value::as_bool)
        == Some(true);
    let (actions, args) = if let Some(actions) = actions.as_array() {
        (actions, Value::Array(Vec::new()))
    } else if let Some(object) = actions.as_object() {
        let Some(actions) = object.get("actions").and_then(Value::as_array) else {
            eprintln!("process-extension: peer config needs an action array");
            return ExitCode::from(2);
        };
        (
            actions,
            object
                .get("args")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new())),
        )
    } else {
        eprintln!("process-extension: peer config must be an action array or object");
        return ExitCode::from(2);
    };
    if term_marker {
        let marker = args
            .as_array()
            .and_then(|args| args.get(1))
            .and_then(Value::as_str);
        let Some(marker) = marker else {
            eprintln!("process-extension: term_marker needs args[1] as its path");
            return ExitCode::from(2);
        };
        if let Err(err) = install_term_marker(marker) {
            eprintln!("process-extension: {err}");
            return ExitCode::FAILURE;
        }
    }
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    let mut variables = Variables::new();
    variables.insert("args".to_owned(), args);
    match run_actions(actions, &mut input, &mut output, &mut variables) {
        Ok(Flow::Continue | Flow::Eof) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("process-extension: peer: {err}");
            ExitCode::FAILURE
        }
    }
}

#[allow(
    unsafe_code,
    reason = "the SIGTERM callback uses only async-signal-safe operations"
)]
fn install_term_marker(path: &str) -> Result<(), String> {
    let path = CString::new(path).map_err(|err| format!("term marker path contains NUL: {err}"))?;
    // SAFETY: the callback reads a live CString and calls only open, write and close.
    let registered = unsafe {
        signal_hook::low_level::register(signal_hook::consts::SIGTERM, move || {
            write_term_marker(&path);
        })
    };
    registered
        .map(|_| ())
        .map_err(|err| format!("register SIGTERM marker: {err}"))
}

/// Writes the fixed marker using only async-signal-safe libc calls.
#[allow(
    unsafe_code,
    reason = "SIGTERM handler writes its marker with async-signal-safe libc calls"
)]
fn write_term_marker(path: &CString) {
    const MARKER: &[u8] = b"{\"termed\":true}\n";
    // SAFETY: `path` is a pre-built CString kept alive by the registered handler.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
            0o600,
        )
    };
    if fd < 0 {
        return;
    }
    // SAFETY: `fd` is open and MARKER is a valid fixed buffer for this write.
    let _written = unsafe { libc::write(fd, MARKER.as_ptr().cast(), MARKER.len()) };
    // SAFETY: `fd` was returned by open above and is still owned here.
    let _closed = unsafe { libc::close(fd) };
}

fn run_actions(
    actions: &[Value],
    input: &mut PeerInput<'_>,
    output: &mut impl Write,
    variables: &mut Variables,
) -> Result<Flow, String> {
    for action in actions {
        match run_action(action, input, output, variables)? {
            Flow::Continue => {}
            Flow::Eof => return Ok(Flow::Eof),
        }
    }
    Ok(Flow::Continue)
}

fn run_action(
    action: &Value,
    input: &mut PeerInput<'_>,
    output: &mut impl Write,
    variables: &mut Variables,
) -> Result<Flow, String> {
    let object = action
        .as_object()
        .ok_or_else(|| "each action must be an object".to_owned())?;
    if object.len() != 1 {
        return Err("each action must have exactly one operation".to_owned());
    }
    let Some((operation, config)) = object.iter().next() else {
        return Err("each action must have one operation".to_owned());
    };
    match operation.as_str() {
        "send" => {
            let line = resolve(config, variables)?;
            write_json_line(output, &line)?;
        }
        "send_raw" => {
            let line = config
                .as_str()
                .ok_or_else(|| "send_raw needs a string".to_owned())?;
            output
                .write_all(line.as_bytes())
                .and_then(|()| output.write_all(b"\n"))
                .and_then(|()| output.flush())
                .map_err(|err| format!("write raw line: {err}"))?;
        }
        "read" => return read_action(config, input, variables),
        "read_until" => return read_until_action(config, input, variables),
        "write_file" | "append_file" => {
            write_file_action(operation == "append_file", config, variables)?;
        }
        "repeat_string" => repeat_string_action(config, variables)?,
        "repeat" => return repeat_action(config, input, output, variables),
        "loop" => {
            let actions = config
                .as_array()
                .ok_or_else(|| "loop needs an action array".to_owned())?;
            loop {
                if run_actions(actions, input, output, variables)? == Flow::Eof {
                    return Ok(Flow::Eof);
                }
            }
        }
        "if" => return if_action(config, input, output, variables),
        "hold" => return hold_action(config, input, output, variables),
        "wait_silent" => wait_silent(config, input)?,
        "spawn_descendant" => spawn_descendant(config)?,
        "linger" => loop {
            std::thread::park();
        },
        "exit" => return Ok(Flow::Eof),
        other => return Err(format!("unknown action `{other}`")),
    }
    Ok(Flow::Continue)
}

fn read_action(
    config: &Value,
    input: &mut PeerInput<'_>,
    variables: &mut Variables,
) -> Result<Flow, String> {
    let config = config
        .as_object()
        .ok_or_else(|| "read needs an object".to_owned())?;
    let Some(value) = read_json_line(input)? else {
        return Ok(Flow::Eof);
    };
    if let Some(expected) = config.get("expect") {
        let expected = resolve(expected, variables)?;
        if !contains(&value, &expected) {
            return Err(format!("read {value} did not contain {expected}"));
        }
    }
    store_read(config, value, variables)?;
    Ok(Flow::Continue)
}

fn read_until_action(
    config: &Value,
    input: &mut PeerInput<'_>,
    variables: &mut Variables,
) -> Result<Flow, String> {
    let config = config
        .as_object()
        .ok_or_else(|| "read_until needs an object".to_owned())?;
    let name = config
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "read_until needs a path".to_owned())?;
    let expected = resolve(
        config
            .get("equals")
            .ok_or_else(|| "read_until needs equals".to_owned())?,
        variables,
    )?;
    loop {
        let Some(value) = read_json_line(input)? else {
            return Ok(Flow::Eof);
        };
        let matched = path(&value, name).is_some_and(|actual| actual == &expected);
        if matched {
            store_read(config, value.clone(), variables)?;
            if config.get("append_to").and_then(Value::as_str).is_some() {
                append_value(config, value, variables)?;
            }
            return Ok(Flow::Continue);
        }
        if config.get("append_to").and_then(Value::as_str).is_some() {
            append_value(config, value, variables)?;
        }
    }
}

fn append_value(
    config: &Map<String, Value>,
    value: Value,
    variables: &mut Variables,
) -> Result<(), String> {
    let Some(name) = config.get("append_to").and_then(Value::as_str) else {
        return Ok(());
    };
    let list = variables
        .entry(name.to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(list) = list.as_array_mut() else {
        return Err(format!("`{name}` is not an array"));
    };
    list.push(value);
    Ok(())
}

fn store_read(
    config: &Map<String, Value>,
    value: Value,
    variables: &mut Variables,
) -> Result<(), String> {
    variables.insert("last".to_owned(), value.clone());
    if let Some(name) = config.get("as").and_then(Value::as_str) {
        variables.insert(name.to_owned(), value.clone());
    }
    append_value(config, value, variables)
}

fn repeat_string_action(config: &Value, variables: &mut Variables) -> Result<(), String> {
    let object = config
        .as_object()
        .ok_or_else(|| "repeat_string needs an object".to_owned())?;
    let name = object
        .get("as")
        .and_then(Value::as_str)
        .ok_or_else(|| "repeat_string needs a variable name".to_owned())?;
    let value = object
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| "repeat_string needs a string value".to_owned())?;
    let count = object
        .get("count")
        .and_then(Value::as_u64)
        .ok_or_else(|| "repeat_string needs a non-negative count".to_owned())?;
    let count =
        usize::try_from(count).map_err(|_| "repeat_string count is too large".to_owned())?;
    if count > 1_000_000 {
        return Err("repeat_string count exceeds 1000000".to_owned());
    }
    variables.insert(name.to_owned(), Value::String(value.repeat(count)));
    Ok(())
}

fn repeat_action(
    config: &Value,
    input: &mut PeerInput<'_>,
    output: &mut impl Write,
    variables: &mut Variables,
) -> Result<Flow, String> {
    let object = config
        .as_object()
        .ok_or_else(|| "repeat needs an object".to_owned())?;
    let count = object
        .get("count")
        .and_then(Value::as_u64)
        .ok_or_else(|| "repeat needs a non-negative count".to_owned())?;
    let count = usize::try_from(count).map_err(|_| "repeat count is too large".to_owned())?;
    let actions = object
        .get("actions")
        .and_then(Value::as_array)
        .ok_or_else(|| "repeat needs an action array".to_owned())?;
    for _ in 0..count {
        if run_actions(actions, input, output, variables)? == Flow::Eof {
            return Ok(Flow::Eof);
        }
    }
    Ok(Flow::Continue)
}

fn if_action(
    config: &Value,
    input: &mut PeerInput<'_>,
    output: &mut impl Write,
    variables: &mut Variables,
) -> Result<Flow, String> {
    let object = config
        .as_object()
        .ok_or_else(|| "if needs an object".to_owned())?;
    let name = object
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "if needs a path".to_owned())?;
    let expected = resolve(
        object
            .get("equals")
            .ok_or_else(|| "if needs equals".to_owned())?,
        variables,
    )?;
    let selected = if reference_value(name, variables).is_some_and(|actual| actual == &expected) {
        object.get("then")
    } else {
        object.get("else")
    };
    if let Some(actions) = selected.and_then(Value::as_array) {
        run_actions(actions, input, output, variables)
    } else {
        Ok(Flow::Continue)
    }
}

fn hold_action(
    config: &Value,
    input: &mut PeerInput<'_>,
    output: &mut impl Write,
    variables: &mut Variables,
) -> Result<Flow, String> {
    let object = config
        .as_object()
        .ok_or_else(|| "hold needs an object".to_owned())?;
    if let Some(ack) = object.get("ack") {
        write_json_line(output, &resolve(ack, variables)?)?;
    }
    match object
        .get("release_on")
        .and_then(Value::as_str)
        .unwrap_or("eof")
    {
        "eof" => {
            while read_json_line(input)?.is_some() {}
            Ok(Flow::Eof)
        }
        "line" => {
            let Some(line) = read_json_line(input)? else {
                return Err("hold ended before its release line".to_owned());
            };
            if let Some(expected) = object.get("release_line") {
                let expected = resolve(expected, variables)?;
                if !contains(&line, &expected) {
                    return Err(format!("release line {line} did not contain {expected}"));
                }
            }
            variables.insert("last".to_owned(), line);
            Ok(Flow::Continue)
        }
        "never" => loop {
            std::thread::park();
        },
        mode => Err(format!("unknown hold release mode `{mode}`")),
    }
}

fn wait_silent(config: &Value, input: &PeerInput<'_>) -> Result<(), String> {
    let timeout_ms = config
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| "wait_silent needs a non-negative timeout_ms".to_owned())?;
    let timeout = std::time::Duration::from_millis(timeout_ms);
    let timeout = rustix::event::Timespec::try_from(timeout)
        .map_err(|err| format!("convert wait_silent timeout: {err}"))?;
    if !input.buffer().is_empty() {
        return Err("stdin already had bytes while waiting for silence".to_owned());
    }
    let stdin = std::io::stdin();
    let mut fds = [rustix::event::PollFd::from_borrowed_fd(
        stdin.as_fd(),
        rustix::event::PollFlags::IN,
    )];
    match rustix::event::poll(&mut fds, Some(&timeout)) {
        Ok(0) => Ok(()),
        Ok(_) => Err("stdin became readable while waiting for silence".to_owned()),
        Err(err) => Err(format!("wait_silent poll: {err}")),
    }
}

fn write_file_action(append: bool, config: &Value, variables: &Variables) -> Result<(), String> {
    let object = config
        .as_object()
        .ok_or_else(|| "file action needs an object".to_owned())?;
    let path = resolve(
        object
            .get("path")
            .ok_or_else(|| "file action needs a path".to_owned())?,
        variables,
    )?;
    let path = path
        .as_str()
        .ok_or_else(|| "file path must be a string".to_owned())?;
    let value = resolve(
        object
            .get("value")
            .ok_or_else(|| "file action needs a value".to_owned())?,
        variables,
    )?;
    let text =
        serde_json::to_string(&value).map_err(|err| format!("serialize file value: {err}"))?;
    if append {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|err| format!("open {path}: {err}"))?;
        writeln!(file, "{text}").map_err(|err| format!("append {path}: {err}"))?;
    } else {
        fs::write(path, text).map_err(|err| format!("write {path}: {err}"))?;
    }
    Ok(())
}

fn spawn_descendant(config: &Value) -> Result<(), String> {
    let config = config
        .as_object()
        .ok_or_else(|| "spawn_descendant needs an object".to_owned())?;
    let mode = config
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| "spawn_descendant needs a mode".to_owned())?;
    if mode != "sleep" {
        return Err(format!("unknown descendant mode `{mode}`"));
    }
    let mut command = Command::new("sleep");
    command
        .arg("60")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    drop(
        command
            .spawn()
            .map_err(|err| format!("spawn descendant: {err}"))?,
    );
    Ok(())
}

fn read_json_line(input: &mut impl BufRead) -> Result<Option<Value>, String> {
    let mut line = String::new();
    match input.read_line(&mut line) {
        Ok(0) => Ok(None),
        Ok(_) => serde_json::from_str(&line)
            .map(Some)
            .map_err(|err| format!("parse input line: {err}")),
        Err(err) => Err(format!("read input line: {err}")),
    }
}

fn write_json_line(output: &mut impl Write, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *output, value).map_err(|err| format!("serialize line: {err}"))?;
    output
        .write_all(b"\n")
        .and_then(|()| output.flush())
        .map_err(|err| format!("flush line: {err}"))
}

fn resolve(value: &Value, variables: &Variables) -> Result<Value, String> {
    if let Some(object) = value.as_object() {
        if object.len() == 1
            && let Some(reference) = object.get("$ref").and_then(Value::as_str)
        {
            return reference_value(reference, variables)
                .cloned()
                .ok_or_else(|| format!("unknown reference `{reference}`"));
        }
        if object.len() == 1
            && let Some(reference) = object.get("$json").and_then(Value::as_str)
        {
            let text = reference_value(reference, variables)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("reference `{reference}` is not JSON text"))?;
            return serde_json::from_str(text)
                .map_err(|err| format!("parse JSON reference `{reference}`: {err}"));
        }
        let mut resolved = Map::new();
        for (key, value) in object {
            resolved.insert(key.clone(), resolve(value, variables)?);
        }
        return Ok(Value::Object(resolved));
    }
    if let Some(array) = value.as_array() {
        return array
            .iter()
            .map(|value| resolve(value, variables))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array);
    }
    Ok(value.clone())
}

fn reference_value<'a>(reference: &str, variables: &'a Variables) -> Option<&'a Value> {
    let mut parts = reference.split('.');
    let first = parts.next()?;
    let mut value = variables.get(first)?;
    for part in parts {
        value = path(value, part)?;
    }
    Some(value)
}

fn path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = if let Ok(index) = part.parse::<usize>() {
            current.as_array()?.get(index)?
        } else {
            current.as_object()?.get(part)?
        };
    }
    Some(current)
}

fn contains(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => {
            expected.iter().all(|(key, expected)| {
                actual
                    .get(key)
                    .is_some_and(|actual| contains(actual, expected))
            })
        }
        (actual, expected) => actual == expected,
    }
}
