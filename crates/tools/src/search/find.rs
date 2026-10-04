//! The `find` search: list what the walk finds (`docs/tools.md`, "Search").

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::fallback;
use super::walk::{self, Root};
use super::{Out, Outcome};

/// Runs `find` against the process's working directory and standard streams,
/// returning the exit code: 0 when the walk finished, 2 on an error. A call
/// the built-in does not handle replaces the process with the system `find`
/// and never returns on success.
pub fn find_main(args: Vec<OsString>) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let stdout = std::io::stdout();
    let mut buffered = std::io::BufWriter::new(stdout.lock());
    let mut stderr = std::io::stderr().lock();
    let outcome = run(&cwd, &args, &mut buffered, &mut stderr);
    buffered.flush().unwrap_or(());
    match outcome {
        Outcome::Done(code) => code,
        Outcome::Fallback => fallback::exec("find", &args, &mut stderr),
    }
}

/// Runs `find` against `cwd`, printing paths to `stdout` and complaints to
/// `stderr`, without touching the process's own streams.
/// `Outcome::Fallback` is returned before anything is read or written.
pub(crate) fn run(
    cwd: &Path,
    args: &[OsString],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Outcome {
    let roots = match parse(args) {
        Parse::Run(roots) => roots,
        Parse::Fallback => return Outcome::Fallback,
    };
    let mut out = Out::new(stdout);
    let mut failed = false;
    for root in &roots {
        if out.broken() {
            break;
        }
        failed |= visit(cwd, root, &mut out, stderr);
    }
    out.flush();
    Outcome::Done(if failed { 2 } else { 0 })
}

/// What `find` was asked to list, or the decision to hand the call to the
/// system tool.
enum Parse {
    /// The search roots as given; no path means `.`.
    Run(Vec<PathBuf>),
    /// A flag or primary the built-in does not handle.
    Fallback,
}

/// Splits the leading paths from the expression. Any expression token hands
/// the call to the system `find`; primaries arrive with the expression.
fn parse(args: &[OsString]) -> Parse {
    let mut roots = Vec::new();
    for arg in args {
        if arg
            .as_encoded_bytes()
            .first()
            .is_some_and(|byte| *byte == b'-')
        {
            return Parse::Fallback;
        }
        roots.push(PathBuf::from(arg));
    }
    if roots.is_empty() {
        roots.push(PathBuf::from("."));
    }
    Parse::Run(roots)
}

/// Prints one search root and what the walk finds below it, without
/// following links. Returns whether anything failed.
fn visit(cwd: &Path, root: &Path, out: &mut Out<'_>, stderr: &mut dyn Write) -> bool {
    match walk::root_of(cwd, root) {
        Err(error) => {
            complaint(stderr, root, &walk::io_message(&error));
            true
        }
        Ok(Root::File(file)) => {
            emit(out, &file.show);
            false
        }
        Ok(Root::Dir(dir)) => {
            emit(out, &dir.show);
            let mut failed = false;
            for entry in walk::walk(cwd, &dir, None) {
                match entry {
                    Ok(found) => {
                        // The root already printed.
                        if found.depth == 0 {
                            continue;
                        }
                        emit(out, &found.display);
                    }
                    Err(error) => {
                        complaint(stderr, &error.display, &error.message);
                        failed = true;
                    }
                }
            }
            failed
        }
    }
}

/// Prints one path and its line ending as bytes, so names outside UTF-8
/// print as they are.
fn emit(out: &mut Out<'_>, path: &Path) {
    out.emit(path.as_os_str().as_encoded_bytes());
    out.emit(b"\n");
}

/// Complains to standard error, as GNU does: `find: '<path>': <reason>`.
fn complaint(stderr: &mut dyn Write, path: &Path, message: &str) {
    stderr.write_all(b"find: '").unwrap_or(());
    stderr
        .write_all(path.as_os_str().as_encoded_bytes())
        .unwrap_or(());
    writeln!(stderr, "': {message}").unwrap_or(());
}

#[cfg(test)]
#[path = "find_tests.rs"]
mod tests;
