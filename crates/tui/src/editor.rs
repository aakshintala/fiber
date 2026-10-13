//! `open_in_editor`, Ctrl+G: the draft, or a paste token, in `$VISUAL` or
//! `$EDITOR` (`docs/tui.md`, "The input box", "Bindings").

use std::ffi::c_int;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use contract::clock::Clock;
use rustix::process::{Pid, Signal};
use rustix::termios::{tcgetpgrp, tcsetpgrp};
use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};

/// The notice when neither variable names an editor.
pub(crate) const NO_EDITOR: &str = "Set $VISUAL or $EDITOR to edit the draft.";

/// The notice when neither variable names an editor for a file.
pub(crate) const NO_EDITOR_FILE: &str = "Set $VISUAL or $EDITOR to open the file.";

/// How many names [`TempFile::create`] tries before giving up.
const ATTEMPTS: usize = 16;

/// How long one wait poll sleeps: a shutdown signal ends the wait within
/// this after arriving, and an exited editor is noticed within this.
const POLL: Duration = Duration::from_millis(20);

/// Grace polls a TERM'd editor tree gets before SIGKILL: 800 ms, the grace
/// `docs/tools.md` gives a stop ("Stopping a command"). An injected clock
/// advances through them at once, so tests never wait it out.
const TERM_GRACE_POLLS: u32 = 40;

/// Polls confirming the editor group emptied after SIGKILL: 2 s, the drain
/// `docs/tools.md` allows ("Stopping a command"). Worst case the shutdown
/// waits 2.8 s here, inside its bound.
const KILL_CONFIRM_POLLS: u32 = 100;

/// How an editor run ended: what the editor returned, or the shutdown
/// signal that ended the wait while the editor still ran.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome<T> {
    /// The editor exited: its result.
    Done(T),
    /// SIGTERM or SIGHUP arrived first: the signal's number.
    Stopped(c_int),
}

/// The next temporary file's number in this process.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// What the editor opens, and where its text goes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// The whole draft, every token expanded.
    Draft,
    /// The paste token with this number.
    Token(usize),
    /// A focused conversation item, read only: what the editor returns
    /// is dropped.
    Item,
}

/// The editor command: `$VISUAL` when set and not empty, else `$EDITOR`
/// when set and not empty, read through `var`.
pub(crate) fn command(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(var)
        .find(|value| !value.trim().is_empty())
}

/// Runs `command` on a new temporary file holding `text`, in the
/// foreground with this process's stdio, and returns the file's text
/// afterwards. The file holds the text and one line break, and one
/// trailing line break is dropped from what is read back, so a file left
/// as it was returns the same text. An error is the notice naming the
/// cause; the file is removed on every path. A shutdown signal ends the
/// wait with [`Outcome::Stopped`]: the editor tree is ended and the draft
/// is unchanged.
pub(crate) fn run(
    command: &str,
    text: &str,
    clock: &Arc<dyn Clock>,
) -> Outcome<Result<String, String>> {
    run_in(&std::env::temp_dir(), command, text, clock)
}

/// [`run`] with the temporary file in `dir`.
fn run_in(
    dir: &Path,
    command: &str,
    text: &str,
    clock: &Arc<dyn Clock>,
) -> Outcome<Result<String, String>> {
    let created = |err: io::Error| format!("Could not write a file for the editor: {err}");
    let (temp, mut file) = match TempFile::create(dir) {
        Ok(created) => created,
        Err(err) => return Outcome::Done(Err(created(err))),
    };
    let failed = |err: io::Error| Outcome::Done(Err(created(err)));
    if let Err(err) = file
        .write_all(text.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.flush())
    {
        return failed(err);
    }
    drop(file);
    let forwarding = catch_for_editor();
    // `sh` splits the command, so a value with arguments such as `code -w`
    // works; the path goes in as `$1`, never through the split.
    let mut child = match Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("{command} \"$1\""))
        .arg("sh")
        .arg(&temp.0)
        // Its own process group, as a shutdown signal ends the run: the
        // wait signals the group, so the shell's children end with it
        // (`crate::clipboard::pipe` and the viewer do the same).
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            return Outcome::Done(Err(format!("Could not start the editor: {err}")));
        }
    };
    let pgid = child.id();
    // The editor reads the terminal, which stops a background group with
    // SIGTTIN: lend it the foreground while it runs, best effort.
    let foreground = Foreground::take(pgid);
    let status = match wait(&mut child, clock, pgid, &forwarding) {
        Ok(Waited::Exited(status)) => status,
        Ok(Waited::Stopped(signal)) => {
            drop(foreground);
            return Outcome::Stopped(signal);
        }
        Err(err) => {
            return Outcome::Done(Err(format!("Could not start the editor: {err}")));
        }
    };
    drop(foreground);
    if let Some(signal) = status.signal() {
        return Outcome::Done(Err(format!(
            "The editor was ended by signal {signal}; the draft is unchanged."
        )));
    }
    if !status.success() {
        let code = status.code().unwrap_or_default();
        return Outcome::Done(Err(format!(
            "The editor exited with status {code}; the draft is unchanged."
        )));
    }
    let mut edited = match fs::read_to_string(&temp.0) {
        Ok(edited) => edited,
        Err(err) => {
            return Outcome::Done(Err(format!("Could not read the edited file: {err}")));
        }
    };
    if edited.ends_with('\n') {
        edited.pop();
    }
    Outcome::Done(Ok(edited))
}

/// Runs `command` on `path`, in the foreground with this process's stdio:
/// Ctrl+G on a configuration view's file (`docs/tui.md`, "Swapped
/// views"). The file need not exist; the editor creates it. An error is
/// the notice naming the cause. A shutdown signal ends the wait with
/// [`Outcome::Stopped`]: the editor tree is ended.
pub(crate) fn open(
    command: &str,
    path: &Path,
    clock: &Arc<dyn Clock>,
) -> Outcome<Result<(), String>> {
    let forwarding = catch_for_editor();
    let mut child = match Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("{command} \"$1\""))
        .arg("sh")
        .arg(path)
        // Its own process group, as a shutdown signal ends the run: the
        // wait signals the group, so the shell's children end with it.
        .process_group(0)
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            return Outcome::Done(Err(format!("Could not start the editor: {err}")));
        }
    };
    let pgid = child.id();
    // As in [`run_in`]: the editor reads the terminal, which stops a
    // background group with SIGTTIN.
    let foreground = Foreground::take(pgid);
    let status = match wait(&mut child, clock, pgid, &forwarding) {
        Ok(Waited::Exited(status)) => status,
        Ok(Waited::Stopped(signal)) => {
            drop(foreground);
            return Outcome::Stopped(signal);
        }
        Err(err) => {
            return Outcome::Done(Err(format!("Could not start the editor: {err}")));
        }
    };
    drop(foreground);
    if let Some(signal) = status.signal() {
        return Outcome::Done(Err(format!("The editor was ended by signal {signal}.")));
    }
    if !status.success() {
        let code = status.code().unwrap_or_default();
        return Outcome::Done(Err(format!("The editor exited with status {code}.")));
    }
    Outcome::Done(Ok(()))
}

/// How the editor wait ended: the editor's exit status, or the shutdown
/// signal that ended the wait while the editor still ran.
enum Waited {
    /// The editor exited: its status.
    Exited(std::process::ExitStatus),
    /// SIGTERM or SIGHUP arrived first: the signal's number.
    Stopped(c_int),
}

/// Waits for the editor `child` to exit, polling with `clock`. The shell
/// runs in its own process group `pgid`, its id, so a shutdown signal
/// ends the whole editor tree: the wait signals the group and reaps the
/// shell, and ends with the signal's number, so the loop quits with its
/// code. SIGINT and SIGQUIT keep their behaviour: the terminal's
/// foreground signals reach this process, and the wait forwards each one
/// caught (`int`, `quit`) to the editor's group, so Ctrl+C or Ctrl+\ typed
/// while the editor has the terminal in cooked mode still ends only the
/// editor, and its death by one reads back as its signal, as before.
fn wait(
    child: &mut Child,
    clock: &Arc<dyn Clock>,
    pgid: u32,
    forward: &Forwarding,
) -> io::Result<Waited> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Waited::Exited(status));
        }
        // Co-pending shutdown signals resolve in no guaranteed order,
        // as on the loop's signal thread: the wait takes whichever flag
        // it observes first.
        let stopped = if forward.term.swap(false, Ordering::SeqCst) {
            SIGTERM
        } else if forward.hup.swap(false, Ordering::SeqCst) {
            SIGHUP
        } else {
            0
        };
        if stopped != 0 {
            // Ends the whole tree: the shell alone may have children of
            // its own (an editor command ending `; :` leaves them behind
            // a killed shell), and the shell's group holds only this
            // run. Best effort: the editor may have exited already, and
            // its status below is dropped either way.
            match support::group::signal(pgid, Signal::TERM) {
                Ok(()) | Err(_) => {}
            }
            // A stopped descendant or one ignoring SIGTERM survives it:
            // force the group down after the grace.
            for _ in 0..TERM_GRACE_POLLS {
                if !support::group::alive(pgid) {
                    break;
                }
                clock.sleep(POLL);
            }
            if support::group::alive(pgid) {
                match support::group::signal(pgid, Signal::KILL) {
                    Ok(()) | Err(_) => {}
                }
            }
            // Reap our shell first: what remains is orphaned to init,
            // then confirm it drained before the terminal comes back, so
            // nothing writes to the restored screen.
            match child.kill() {
                Ok(_) | Err(_) => {}
            }
            drop(child.wait());
            for _ in 0..KILL_CONFIRM_POLLS {
                if !support::group::alive(pgid) {
                    break;
                }
                clock.sleep(POLL);
            }
            return Ok(Waited::Stopped(stopped));
        }
        for (caught, signal) in [(&forward.int, Signal::INT), (&forward.quit, Signal::QUIT)] {
            if caught.swap(false, Ordering::SeqCst) {
                match support::group::signal(pgid, signal) {
                    Ok(()) | Err(_) => {}
                }
            }
        }
        clock.sleep(POLL);
    }
}

/// This run's signal forwarding: the flags [`wait`] polls and the
/// registrations, unregistered when the run ends. Dropping the ids never
/// unregisters them, so without the [`Drop`] below every run would leak a
/// callback and a flag.
struct Forwarding {
    int: Arc<AtomicBool>,
    quit: Arc<AtomicBool>,
    term: Arc<AtomicBool>,
    hup: Arc<AtomicBool>,
    held: Vec<signal_hook::SigId>,
}

impl Drop for Forwarding {
    fn drop(&mut self) {
        for id in self.held.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

/// Registers this run's forwarding: SIGINT and SIGQUIT to the editor's
/// group, and the shutdown signals for [`wait`]'s own end. The flags the
/// [`Forwarding`] guard unregisters when the run ends. Registered before
/// the spawn, so an acknowledged editor implies installed handlers: an
/// interrupt raised after the acknowledgement is always forwarded, and a
/// shutdown signal always ends the wait, never missed.
fn catch_for_editor() -> Forwarding {
    let mut held = Vec::new();
    let mut flag = |signal| {
        let caught = Arc::new(AtomicBool::new(false));
        if let Ok(id) = signal_hook::flag::register(signal, Arc::clone(&caught)) {
            held.push(id);
        }
        caught
    };
    let int = flag(SIGINT);
    let quit = flag(SIGQUIT);
    let term = flag(SIGTERM);
    let hup = flag(SIGHUP);
    Forwarding {
        int,
        quit,
        term,
        hup,
        held,
    }
}

/// The editor's foreground ownership of the controlling terminal, restored
/// when dropped: an editor in a background group stops on its first
/// terminal read (`SIGTTIN`), so the run lends it the foreground and takes
/// it back afterwards. Best effort throughout: with no controlling
/// terminal, or one another group owns, there is nothing to lend.
struct Foreground {
    tty: OwnedFd,
    previous: Pid,
}

impl Foreground {
    /// Lends the controlling terminal's foreground to `pgid` when this
    /// process holds it, for the guard's life. `None` without a controlling
    /// terminal, when another group holds the foreground, when `pgid` is
    /// no pid, or when the transfer fails: the editor still runs, only
    /// without foreground reads.
    fn take(pgid: u32) -> Option<Self> {
        let tty: OwnedFd = std::fs::File::open("/dev/tty").ok()?.into();
        if tcgetpgrp(&tty).ok()? != own_foreground_holder(&tty)? {
            return None;
        }
        let target = Pid::from_raw(pgid.try_into().ok()?)?;
        let previous = give(&tty, target)?;
        Some(Self { tty, previous })
    }
}

impl Drop for Foreground {
    fn drop(&mut self) {
        // Best effort: the handover retakes the screen either way, and a
        // failed restore leaves the foreground on a dead group, which the
        // next transfer repairs when it reads the holder back.
        without_ttou_stop(|| tcsetpgrp(&self.tty, self.previous).ok());
    }
}

/// Runs `run` with SIGTTOU caught: a foreground change from the
/// background stops the process, as shells know, so both directions of
/// the editor's lending catch it and restore the disposition after. A
/// caught flag counts the same as ignored, needs no unsafe, and
/// unregisters after. Best effort: without a handler installed the
/// change runs unguarded.
fn without_ttou_stop<R>(run: impl FnOnce() -> R) -> R {
    use signal_hook::consts::SIGTTOU;
    let guard = signal_hook::flag::register(SIGTTOU, Arc::new(AtomicBool::new(false))).ok();
    let out = run();
    if let Some(id) = guard {
        signal_hook::low_level::unregister(id);
    }
    out
}

/// This process's group when it owns the terminal `tty`'s foreground, so
/// [`Foreground::take`] lends only what is ours to lend. `None` when
/// another group owns it.
fn own_foreground_holder(tty: &OwnedFd) -> Option<Pid> {
    let holder = tcgetpgrp(tty).ok()?;
    let ours = rustix::process::getpgid(None).ok()?;
    (holder == ours).then_some(holder)
}

/// Sets the foreground group of the terminal `tty` to `pgid`, returning
/// the previous one once confirmed. `None` when `tty` is no terminal or
/// the transfer fails.
fn give(tty: &OwnedFd, pgid: Pid) -> Option<Pid> {
    let previous = tcgetpgrp(tty).ok()?;
    without_ttou_stop(|| tcsetpgrp(tty, pgid).ok())?;
    // Confirm: a racing foreground change must not read back as ours.
    if tcgetpgrp(tty).ok()? == pgid {
        Some(previous)
    } else {
        without_ttou_stop(|| tcsetpgrp(tty, previous).ok());
        None
    }
}

/// The temporary file the editor opens, removed when dropped.
struct TempFile(PathBuf);

impl TempFile {
    /// Creates `fiber-draft-<pid>-<n>.md` in `dir`, new and readable by
    /// this user alone, trying the next `n` when the name is taken.
    fn create(dir: &Path) -> io::Result<(Self, File)> {
        let mut taken = None;
        for _ in 0..ATTEMPTS {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("fiber-draft-{}-{n}.md", std::process::id()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => return Ok((Self(path), file)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => taken = Some(err),
                Err(err) => return Err(err),
            }
        }
        Err(taken.unwrap_or_else(|| io::Error::other("no free name")))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        fs::remove_file(&self.0).unwrap_or(());
    }
}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;
