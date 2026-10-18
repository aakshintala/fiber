//! The `connect` jig (`docs/testing.md`, "Jigs"): sends commands typed on
//! stdin to a session socket or a selected hub, printing the answers.
//!
//! `cargo run -p doors --example connect -- <socket path>`
//! `cargo run -p doors --example connect -- [--hub <name>] [--fiber <binary>]`
//! Either flag selects hub mode: --hub names an entry, else hub.default is
//! used, else the local hub. A local target requires --fiber to start it.

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
    let (stream, hello) =
        doors::hub::connect_target(&home, &target, &mut start, clock).map_err(io::Error::other)?;
    let mut stdout = io::stdout();
    serde_json::to_writer(&mut stdout, &hello).map_err(io::Error::other)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    relay(Ok(stream), io::stdin(), &mut stdout)
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
                stdout.write_all(&line)?;
                stdout.write_all(b"\n")?;
            }
        }
    }
    stdout.flush()
}

fn copy_stdin(stdin: impl Read, writer: &mut UnixStream) {
    let mut stdin = BufReader::new(stdin);
    loop {
        match support::framing::read_line(&mut stdin, support::framing::MAX_LINE) {
            Ok(None) | Err(_) => break,
            Ok(Some(buf)) => {
                if writer.write_all(&buf).is_err()
                    || writer.write_all(b"\n").is_err()
                    || writer.flush().is_err()
                {
                    break;
                }
            }
        }
    }
}
