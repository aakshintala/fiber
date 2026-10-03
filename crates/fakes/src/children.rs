//! Child processes that misbehave on purpose (`docs/testing.md`, "Fakes"):
//! ignore SIGTERM, leave descendants, or escape their process group.
//!
//! Each command writes two lines to a FIFO: its process-group id as its first
//! action, then the pids of what it started once that misbehaviour is set up.
//! Descendants close the FIFO. The reader holds its own write end, so the
//! open does not block and a read does not see end-of-file before the
//! command opens the FIFO. On macOS, forking while a read-only open is
//! blocked in the kernel returns end-of-file. A command that dies before a
//! line is caught by [`Ready::wait`]'s deadline.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

/// How long [`Ready::new`] waits for its reader to open the FIFO.
const OPEN_DEADLINE: Duration = Duration::from_secs(5);

enum Line {
    Text(String),
    End,
}

/// A FIFO a misbehaving command writes its ready lines to.
pub struct Ready {
    path: PathBuf,
    lines: mpsc::Receiver<Line>,
}

impl Ready {
    /// Creates `ready.fifo` in `dir` and starts reading it. The read is open
    /// before the command runs, so a line written at once is not lost.
    #[allow(
        clippy::panic,
        reason = "a ready fifo that cannot be created means the test cannot proceed"
    )]
    pub fn new(dir: &Path) -> Self {
        let path = dir.join("ready.fifo");
        match Command::new("mkfifo").arg(&path).status() {
            Ok(status) if status.success() => {}
            Ok(status) => panic!("mkfifo {} exited {status}", path.display()),
            Err(err) => panic!("mkfifo {}: {err}", path.display()),
        }
        let fifo = path.clone();
        let (tx, rx) = mpsc::channel();
        let (opened_tx, opened_rx) = mpsc::channel();
        thread::spawn(move || read_fifo(&fifo, &tx, &opened_tx));
        match opened_rx.recv_timeout(OPEN_DEADLINE) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => panic!(
                "waited {OPEN_DEADLINE:?} for ready fifo {} to open",
                path.display()
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!(
                "ready fifo {} reader stopped before it opened",
                path.display()
            ),
        }
        Self { path, lines: rx }
    }

    /// The FIFO's path, passed into a command string.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The next line's process ids. Panics at `within`, naming the wait, or
    /// when the FIFO closes before a line.
    #[allow(
        clippy::panic,
        reason = "a ready line that never arrives means the test cannot proceed"
    )]
    pub fn wait(&self, within: Duration) -> Vec<u32> {
        match self.lines.recv_timeout(within) {
            Ok(Line::Text(line)) => pids(&line, &self.path),
            Ok(Line::End) => panic!(
                "ready fifo {} closed before a line arrived",
                self.path.display()
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!(
                    "waited {within:?} for a ready line from {}",
                    self.path.display()
                )
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "ready fifo {} closed before a line arrived",
                    self.path.display()
                )
            }
        }
    }
}

fn read_fifo(path: &Path, tx: &Sender<Line>, opened: &Sender<()>) {
    // Read-write returns at once. Holding that end makes the read below wait
    // for the command's bytes. A read-only open would block until a writer,
    // and the caller's fork races that blocked open.
    let files = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .and_then(|hold| File::open(path).map(|file| (hold, file)));
    match opened.send(()) {
        Ok(()) | Err(_) => {}
    }
    let Ok((hold, file)) = files else {
        drop(tx.send(Line::End));
        return;
    };
    let mut reader = BufReader::new(file);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => {
                drop(tx.send(Line::End));
                drop(hold);
                return;
            }
            Ok(_) => {
                if tx.send(Line::Text(line)).is_err() {
                    drop(hold);
                    return;
                }
            }
        }
    }
}

#[allow(
    clippy::panic,
    reason = "a ready line that is not process ids means the test cannot proceed"
)]
fn pids(line: &str, path: &Path) -> Vec<u32> {
    let mut out = Vec::new();
    for word in line.split_whitespace() {
        match word.parse() {
            Ok(pid) => out.push(pid),
            Err(_) => panic!(
                "ready line {line:?} from {} is not process ids",
                path.display()
            ),
        }
    }
    if out.is_empty() {
        panic!("ready line from {} was empty", path.display());
    }
    out
}

/// Ignores SIGTERM and blocks on its block FIFO. The second line is its own
/// pid. A line written to that FIFO is answered with its pid, then it blocks
/// again.
pub fn ignores_sigterm(ready: &Path) -> String {
    let block = quote(&block_of(ready));
    let ready = quote(ready);
    format!(
        "trap '' TERM\necho $$ > {ready}\nmkfifo {block}\necho $$ >> {ready}\nread -r _ < {block}\necho $$ >> {ready}\nread -r _ < {block}\n"
    )
}

/// Starts a child that ignores SIGTERM and SIGHUP, then waits. The shell
/// itself still dies on SIGTERM. The second line is the child's pid, written
/// after its traps are set. SIGHUP is ignored because the shell is the
/// session leader: its death can hang up the group.
pub fn leaves_descendants(ready: &Path) -> String {
    let block = quote(&block_of(ready));
    let ready = quote(ready);
    format!(
        "echo $$ > {ready}\nmkfifo {block}\n/bin/bash -c 'trap \"\" TERM HUP; echo $$ >> \"$1\"; read -r _ < \"$2\"' _ {ready} {block} &\nwait\n"
    )
}

/// Starts a process that leaves the group and holds standard output open.
/// The second line is that process's pid, written after it has left.
pub fn escapes_group(ready: &Path) -> String {
    let ready = quote(ready);
    format!(
        "echo $$ > {ready}\nperl -MPOSIX -e 'POSIX::setsid(); $SIG{{HUP}} = \"IGNORE\"; $SIG{{TERM}} = \"IGNORE\"; open my $f, \">>\", $ARGV[0] or die $!; print $f \"$$\\n\"; close $f; sleep 3600 while 1' {ready} &\nwait\n"
    )
}

fn block_of(ready: &Path) -> PathBuf {
    let mut name = ready
        .file_name()
        .map(OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".block");
    ready.with_file_name(name)
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[cfg(test)]
#[path = "children_tests.rs"]
mod tests;
