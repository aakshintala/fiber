//! `fiber extension install`, `update`, `remove` and `list`
//! (`docs/extensions.md`, "Installing"): install fetches and checks an
//! extension and its dependencies, shows what they register and asks
//! before going ahead in a terminal; update moves one or every installed
//! extension to its newest tag; remove deletes an extension and what
//! nothing else uses; list names what is installed. `main` parses argv
//! and dispatches here; this crate takes plain values.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use contract::ErrorCode;
use contract::clock::Clock;
use contract::shapes::Failure;
use extensions::{Origin, Request};

use crate::{fail, failed};

/// What a command asks on and reports to: whether stdin is a terminal,
/// stdin, and stderr.
struct Io<'a> {
    terminal: bool,
    input: &'a mut dyn BufRead,
    err: &'a mut dyn Write,
}

/// `fiber extension install <name or path>`: fetches and checks the
/// extension and its dependencies, shows what they register and asks
/// when stdin is a terminal, and prints each name installed.
pub fn extension_install(name_or_path: &str, fiber_version: &str, clock: &dyn Clock) -> i32 {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let request = if extensions::is_path(name_or_path) {
                Request::Path(PathBuf::from(name_or_path))
            } else {
                Request::Install(name_or_path.to_owned())
            };
            install(
                &home,
                request,
                fiber_version,
                &Origin::github(),
                clock,
                Io {
                    terminal,
                    input: &mut stdin.lock(),
                    err: &mut io::stderr(),
                },
            )
        });
    match ran {
        Ok(code) => code,
        Err(e) => fail(e),
    }
}

/// `fiber extension update [<name>]`: with a name, reinstalls that
/// extension at its newest version; without one, updates every
/// installed extension a person asked for, in listing order.
pub fn extension_update(name: Option<&str>, fiber_version: &str, clock: &dyn Clock) -> i32 {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let io = Io {
                terminal,
                input: &mut stdin.lock(),
                err: &mut io::stderr(),
            };
            match name {
                Some(name) => install(
                    &home,
                    Request::Update(name.to_owned()),
                    fiber_version,
                    &Origin::github(),
                    clock,
                    io,
                ),
                None => update_all(&home, fiber_version, &Origin::github(), clock, io),
            }
        });
    match ran {
        Ok(code) => code,
        Err(e) => fail(e),
    }
}

/// `fiber extension remove <name>`: removes an extension, the
/// dependencies nothing else uses, and their data and settings, asking
/// first in a terminal.
pub fn extension_remove(name: &str, clock: &dyn Clock) -> i32 {
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            remove(
                &home,
                name,
                clock,
                Io {
                    terminal,
                    input: &mut stdin.lock(),
                    err: &mut io::stderr(),
                },
            )
        });
    match ran {
        Ok(code) => code,
        Err(e) => fail(e),
    }
}

/// `fiber extension list`: one line per installed extension: name,
/// version and commit.
pub fn extension_list(clock: &dyn Clock) -> i32 {
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| list(&home, clock, &mut io::stdout().lock()));
    match ran {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

/// Installs `request`: fetches and checks what it and its dependencies
/// need, shows what they register and asks when the terminal says so,
/// and prints each name installed.
fn install(
    home: &Path,
    request: Request,
    fiber_version: &str,
    origin: &Origin,
    clock: &dyn Clock,
    io: Io<'_>,
) -> Result<i32, Failure> {
    let Io {
        terminal,
        input,
        err,
    } = io;
    let _ = (
        home,
        request,
        fiber_version,
        origin,
        clock,
        terminal,
        input,
        err,
    );
    Err(failed(
        ErrorCode::IoFailed,
        "fiber extension commands are not built in cli yet",
    ))
}

/// Updates every installed extension a person asked for, in listing
/// order, stopping at the first declined install or the first failure.
fn update_all(
    home: &Path,
    fiber_version: &str,
    origin: &Origin,
    clock: &dyn Clock,
    io: Io<'_>,
) -> Result<i32, Failure> {
    let Io {
        terminal,
        input,
        err,
    } = io;
    let _ = (home, fiber_version, origin, clock, terminal, input, err);
    Err(failed(
        ErrorCode::IoFailed,
        "fiber extension commands are not built in cli yet",
    ))
}

/// Removes the extension `typed` names, asking first in a terminal.
fn remove(home: &Path, typed: &str, clock: &dyn Clock, io: Io<'_>) -> Result<i32, Failure> {
    let Io {
        terminal,
        input,
        err,
    } = io;
    let _ = (home, typed, clock, terminal, input, err);
    Err(failed(
        ErrorCode::IoFailed,
        "fiber extension commands are not built in cli yet",
    ))
}

/// Lists what `home` holds: each damaged directory first, then one line
/// per installed extension.
fn list(home: &Path, clock: &dyn Clock, out: &mut dyn Write) -> Result<(), Failure> {
    let _ = (home, clock, out);
    Err(failed(
        ErrorCode::IoFailed,
        "fiber extension commands are not built in cli yet",
    ))
}

#[cfg(test)]
#[path = "extension_tests.rs"]
mod tests;
