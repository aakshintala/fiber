//! The panic hook (`docs/code-quality.md`, "What a panic leaves"): every
//! `fiber` process installs it first, and it writes one crash report before
//! the process aborts.

use std::ffi::OsString;
use std::fs::{DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::panic::PanicHookInfo;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::UNIX_EPOCH;

/// The session later crash files are named by. Unset while no session id
/// exists yet, when the file is named by the process id instead.
static SESSION: OnceLock<String> = OnceLock::new();

/// Installs the panic hook. `fiber_home` and `home` are the values of
/// `FIBER_HOME` and `HOME`, resolved with `config::fiber_home` only when a
/// panic happens, so installing creates nothing. `clock` dates the file.
pub(crate) fn install(
    fiber_home: Option<OsString>,
    home: Option<OsString>,
    clock: Arc<dyn contract::clock::Clock>,
) {
    std::panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        hook(info, &fiber_home, &home, &clock);
    }));
}

/// Names later crash files by this session id. The first call wins; later
/// calls do nothing.
pub(crate) fn attach(id: &contract::SessionId) {
    match SESSION.set(id.0.clone()) {
        Ok(()) | Err(_) => {}
    }
}

/// One panic's report: the file's text and the report on stderr are this,
/// and stderr adds exactly one line after it. The hook runs on the thread
/// that panicked and takes no lock of Fiber's: the session id is one atomic
/// load, and nothing else here is shared with the log. The terminal restore
/// goes at the top when the TUI builds one (`docs/code-quality.md`, "What a
/// panic leaves").
fn hook(
    info: &PanicHookInfo<'_>,
    fiber_home: &Option<OsString>,
    home: &Option<OsString>,
    clock: &Arc<dyn contract::clock::Clock>,
) {
    let thread = std::thread::current();
    let name = thread.name().unwrap_or("<unnamed>");
    let location = info.location().map_or_else(
        || "unknown location".to_owned(),
        |found| format!("{}:{}:{}", found.file(), found.line(), found.column()),
    );
    let backtrace = std::backtrace::Backtrace::force_capture().to_string();
    let reported = report(name, message(info), &location, &backtrace);
    let ms = clock
        .wall()
        .duration_since(UNIX_EPOCH)
        .map(|past| past.as_millis())
        .unwrap_or(0);
    let file = file_name(SESSION.get().map(String::as_str), std::process::id(), ms);
    let stderr = match write_report(fiber_home.clone(), home.clone(), &file, &reported) {
        Ok(path) => format!(
            "{reported}fiber: crash report written to {}\n",
            path.display()
        ),
        Err(reason) => format!("{reported}fiber: no crash report was written: {reason}\n"),
    };
    match std::io::stderr().write_all(stderr.as_bytes()) {
        Ok(()) | Err(_) => {}
    }
    std::process::abort();
}

/// The panic's message: the string payload, or std's wording when the
/// payload is not a string.
fn message<'a>(info: &'a PanicHookInfo<'a>) -> &'a str {
    info.payload_as_str().unwrap_or("Box<dyn Any>")
}

/// The report: the same text goes to the file and to stderr.
fn report(thread: &str, message: &str, location: &str, backtrace: &str) -> String {
    format!("thread '{thread}' panicked at {location}:\n{message}\nstack backtrace:\n{backtrace}\n")
}

/// `<id>-<ms>.txt`, with the process id when no session id is attached.
fn file_name(session: Option<&str>, pid: u32, ms: u128) -> String {
    match session {
        Some(id) => format!("{id}-{ms}.txt"),
        None => format!("{pid}-{ms}.txt"),
    }
}

/// Writes `reported` to `crashes/<file>` under Fiber home, creating a
/// missing home and `crashes/` mode 0700 and the file itself mode 0600. The
/// file's path, or why no file was written.
fn write_report(
    fiber_home: Option<OsString>,
    home: Option<OsString>,
    file: &str,
    reported: &str,
) -> Result<PathBuf, String> {
    let home = config::fiber_home(fiber_home, home).map_err(|e| e.to_string())?;
    let crashes = home.join("crashes");
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&crashes)
        .map_err(|e| e.to_string())?;
    let path = crashes.join(file);
    let mut written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| e.to_string())?;
    written
        .write_all(reported.as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
#[path = "crash_tests.rs"]
mod tests;
