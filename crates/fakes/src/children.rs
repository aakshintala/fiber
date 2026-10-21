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
use std::io::{self, BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use crate::deadline::Deadline;
use crate::{MUST_SUCCEED_WITHIN, Watchdog, group_empties, kill_group};

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
    #[track_caller]
    pub fn new(dir: &Path) -> Self {
        let path = dir.join("ready.fifo");
        crate::fifo(&path);
        Self::at(&path, &|| OPEN_DEADLINE)
    }

    /// Starts reading the existing FIFO `fifo`, as [`Ready::new`] does after
    /// making it. The wait for the reader to open it is bounded by `left()`.
    /// With no time left it panics naming the FIFO, before opening it.
    #[allow(
        clippy::panic,
        reason = "a ready fifo that does not open means the test cannot proceed"
    )]
    #[track_caller]
    pub fn at(fifo: &Path, left: &dyn Fn() -> Duration) -> Self {
        let path = fifo.to_path_buf();
        let within = left();
        if within.is_zero() {
            panic!("no time left to open ready fifo {}", path.display());
        }
        let reading = path.clone();
        let (tx, rx) = mpsc::channel();
        let (opened_tx, opened_rx) = mpsc::channel();
        thread::spawn(move || read_fifo(&reading, &tx, &opened_tx));
        match Deadline::after(within).recv(&opened_rx) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => panic!(
                "waited {within:?} for ready fifo {} to open",
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

    /// The next line's process ids. Panics at `deadline`, naming the wait, or
    /// when the FIFO closes before a line.
    #[allow(
        clippy::panic,
        reason = "a ready line that never arrives means the test cannot proceed"
    )]
    #[track_caller]
    pub fn wait(&self, deadline: Deadline) -> Vec<u32> {
        let within = deadline.left();
        match deadline.recv(&self.lines) {
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
/// pid. Each line written to that FIFO is answered with its pid, then it
/// blocks again. A read that ends at end-of-file, because the writer closed
/// before the next open, writes nothing and blocks again, so the command
/// never exits on its own while the FIFO exists.
pub fn ignores_sigterm(ready: &Path) -> String {
    let block = quote(&block_of(ready));
    let ready = quote(ready);
    format!(
        "trap '' TERM\necho $$ > {ready}\nmkfifo {block}\necho $$ >> {ready}\nwhile :; do if read -r _ < {block}; then echo $$ >> {ready}; fi; [ -p {block} ] || exit 1; done\n"
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

/// Runs `command` in its own process group and collects its output: the
/// one runner every test hands to a credential read
/// (`docs/testing.md`, "Running tests"). The caller configures stdio, as
/// `config` does (stdin and stderr null, stdout piped); this sets only the
/// process group. A watchdog guards the group while the run lasts. The
/// wait is bounded by the test's wall deadline, and the product's timeout
/// is ignored on purpose: bounding by it is `Reads::credential`'s job.
/// After the exit whatever the group still holds is killed with SIGKILL
/// and the group is waited out, so no descendant outlives the call.
///
/// # Panics
///
/// When the run outlasts the wall deadline (naming the program), or when
/// the group does not empty after the kill.
///
/// # Errors
///
/// When the command cannot be started.
#[track_caller]
#[allow(
    clippy::panic,
    reason = "a test runner that hangs or leaks means the test cannot proceed"
)]
pub fn grouped_output(command: &mut Command, _timeout: Duration) -> io::Result<Output> {
    command.process_group(0);
    let program = command.get_program().to_string_lossy().into_owned();
    let child = command.spawn()?;
    let pid = child.id();
    let watchdog = Watchdog::group(pid);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let output = child.wait_with_output();
        match done.send(output) {
            Ok(()) | Err(_) => {}
        }
    });
    let output = match Deadline::after(MUST_SUCCEED_WITHIN).recv(&finished) {
        Ok(output) => output?,
        Err(_) => {
            match kill_group(pid, "KILL") {
                Ok(_) | Err(_) => {}
            }
            panic!("`{program}` did not exit within {MUST_SUCCEED_WITHIN:?}");
        }
    };
    match kill_group(pid, "KILL") {
        Ok(_) | Err(_) => {}
    }
    assert!(
        group_empties(pid, Deadline::after(MUST_SUCCEED_WITHIN)),
        "`{program}` left its process group {pid} behind"
    );
    watchdog.stand_down(Deadline::after(MUST_SUCCEED_WITHIN));
    Ok(output)
}

#[cfg(test)]
#[path = "children_tests.rs"]
mod tests;
