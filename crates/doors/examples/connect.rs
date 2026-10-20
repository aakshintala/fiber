//! The `connect` jig (`docs/testing.md`, "Jigs"): sends commands typed on
//! stdin to a session socket or a selected hub, printing the answers.
//!
//! `cargo run -p doors --example connect -- <socket path>`
//! `cargo run -p doors --example connect -- [--hub <name>] [--fiber <binary>]`
//! Either flag selects hub mode: --hub names an entry, else hub.default is
//! used, else the local hub. A local target requires --fiber to start it.
//! A listed hub answers `hub_hello` at once; one that does not answer
//! within [`doors::hub::CONNECT_DEADLINE`] fails with a stderr line.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a jig's output is its interface"
)]

#[path = "../tests/support/connect_args.rs"]
mod connect_args;
use connect_args::{Args, parse};
use std::io::{self, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::thread;

const USAGE: &str = "usage: cargo run -p doors --example connect -- <socket path>\n       cargo run -p doors --example connect -- [--hub <name>] [--fiber <binary>]";

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(()) => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match args {
        Args::Socket(path) => relay(UnixStream::connect(path), io::stdin(), &mut io::stdout()),
        Args::Hub { hub, fiber } => {
            hub_session(hub.as_deref(), fiber.as_deref(), &fakes::clock::SystemClock)
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("connect: {error}");
            ExitCode::FAILURE
        }
    }
}

fn hub_session(
    named: Option<&str>,
    binary: Option<&str>,
    clock: &dyn contract::clock::Clock,
) -> io::Result<()> {
    let home = config::fiber_home_from_env().map_err(io::Error::other)?;
    let workspace = std::env::current_dir()?;
    let target = doors::hub::load_target(&home, &workspace, named).map_err(io::Error::other)?;
    if !target.is_listed() && binary.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a local hub target requires --fiber <binary>",
        ));
    }
    let mut start = || start_hub(binary.unwrap_or_default());
    // The 5 s bound covers Unix-address listed targets only: they carry no
    // token and skip authentication, so the bound is exactly connect-plus-hello.
    // Any other listed address keeps the legacy connect_target call below.
    let unix_hub = if let doors::hub::Target::Listed { name, address, .. } = &target {
        matches!(address, doors::hub::Address::Unix(_)).then(|| name.clone())
    } else {
        None
    };
    let (stream, hello) = if let Some(hub) = unix_hub {
        connect_listed(&home, target, &hub)?
    } else {
        doors::hub::connect_target(&home, &target, &mut start, clock).map_err(io::Error::other)?
    };
    let mut stdout = io::stdout();
    let hello = serde_json::to_vec(&hello).map_err(io::Error::other)?;
    support::framing::write_line(&mut stdout, &hello)?;
    relay(Ok(stream), io::stdin(), &mut stdout)
}

/// Connects to a listed hub, which answers `hub_hello` at once: the whole
/// connect waits at most [`doors::hub::CONNECT_DEADLINE`], then fails with
/// a stderr line naming the hub instead of hanging silent. A listed hub is
/// never started, so the wait runs on a thread the timeout abandons; the
/// process exits right after, which ends the thread with it.
fn connect_listed(
    home: &std::path::Path,
    target: doors::hub::Target,
    hub: &str,
) -> io::Result<doors::hub::Hub> {
    let hub = hub.to_owned();
    let home = home.to_path_buf();
    let (done, waiting) = std::sync::mpsc::channel();
    thread::Builder::new()
        .name("connect-listed".to_owned())
        .spawn(move || {
            let mut start = || Err(io::Error::other("a listed hub is never started"));
            let _sent = done.send(doors::hub::connect_target(
                &home,
                &target,
                &mut start,
                &fakes::clock::SystemClock,
            ));
        })
        .map_err(io::Error::other)?;
    // One bounded wait through the test deadline type, never a bare receive.
    let bound = fakes::Deadline::after(doors::hub::CONNECT_DEADLINE);
    match bound.recv(&waiting) {
        Ok(result) => result.map_err(io::Error::other),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "the listed hub '{hub}' did not say hub_hello within {} s",
                doors::hub::CONNECT_DEADLINE.as_secs()
            ),
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(format!(
            "the listed hub '{hub}' ended the connect without saying hub_hello"
        ))),
    }
}

/// Starts `<binary> hub serve` in its own process group with null stdio.
/// The hub it starts is reaped by a thread.
fn start_hub(binary: &str) -> io::Result<()> {
    let mut child = Command::new(binary)
        .arg("hub")
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    thread::Builder::new()
        .name("connect-reap".to_owned())
        .spawn(move || match child.wait() {
            Ok(_) | Err(_) => {}
        })
        .map_err(|error| io::Error::new(error.kind(), format!("reap: {error}")))?;
    Ok(())
}

fn relay(
    stream: io::Result<UnixStream>,
    stdin: impl Read + Send + 'static,
    stdout: &mut impl Write,
) -> io::Result<()> {
    let stream = stream?;
    let mut writer = stream.try_clone()?;
    let incoming = thread::Builder::new()
        .name("connect-stdin".to_owned())
        .spawn(move || copy_stdin(stdin, &mut writer))?;
    // The socket closing must not wait on stdin, which may still be blocked.
    drop(incoming);
    let mut reader = BufReader::new(stream);
    loop {
        match support::framing::read_line(&mut reader, support::framing::MAX_LINE) {
            Ok(None) | Err(_) => break,
            Ok(Some(line)) => {
                support::framing::write_line(&mut *stdout, &line)?;
            }
        }
    }
    Ok(())
}

fn copy_stdin(stdin: impl Read, writer: &mut UnixStream) {
    let mut stdin = BufReader::new(stdin);
    loop {
        match support::framing::read_line(&mut stdin, support::framing::MAX_LINE) {
            Ok(None) | Err(_) => break,
            Ok(Some(buf)) => {
                if support::framing::write_line(&mut *writer, &buf).is_err() {
                    break;
                }
            }
        }
    }
}
