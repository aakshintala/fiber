//! One session process's door side (`docs/invocation.md`, "Lifecycle" and
//! "Processes"): its socket at `run/<session_id>` in Fiber home, its event
//! stream copied to stdout, its one prompt, and what is left when it exits.
//! The loop writes the session's lines; this side only watches them.

use std::fs::{self, DirBuilder, Permissions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use contract::events::Event;
use contract::inbox::{Delivery, Message};
use contract::shapes::{ContentPart, Failure, Origin, Sender};
use contract::{CommandId, ErrorCode};
use log::{Log, Watcher};

use crate::{failure, mint};

/// The longest socket path the platform binds: `sun_path` less its
/// terminating byte (`docs/state.md`, "Sockets").
const SOCKET_PATH_MAX: usize = if cfg!(target_os = "macos") { 103 } else { 107 };

/// A running session process's door side: its socket and the thread copying
/// its events to stdout.
pub struct Session {
    dir: PathBuf,
    socket: PathBuf,
    listener: UnixListener,
    printer: JoinHandle<()>,
}

impl Session {
    /// Opens the door side of the session whose directory, just created, is
    /// `dir`: binds its socket in `home`'s `run/` and starts copying every
    /// event `watcher` receives to `out`, one JSON line each. A failure is
    /// one before any session exists, so it deletes `dir`.
    pub fn open(
        home: &Path,
        dir: &Path,
        watcher: Watcher,
        out: Box<dyn Write + Send>,
    ) -> Result<Self, Failure> {
        let opened = bind(home, dir).and_then(|(socket, listener)| {
            let printer = thread::Builder::new()
                .name("stdout".to_owned())
                .spawn(move || print(watcher, out));
            match printer {
                Ok(printer) => Ok(Self {
                    dir: dir.to_owned(),
                    socket,
                    listener,
                    printer,
                }),
                Err(e) => {
                    remove_socket(&socket);
                    Err(failure(
                        ErrorCode::IoFailed,
                        format!("cannot start a thread: {e}"),
                    ))
                }
            }
        });
        if opened.is_err() {
            fs::remove_dir_all(dir).unwrap_or(());
        }
        opened
    }

    /// Runs `fiber ask`'s one turn: hands `run` an inbox holding `prompt`
    /// from a driver and nothing more to come, so the loop `run` starts
    /// stops when the turn ends (`docs/invocation.md`, "Lifecycle").
    pub fn ask(
        &self,
        prompt: String,
        run: impl FnOnce(Receiver<Delivery>) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let (inbox, waiting) = mpsc::channel();
        let message = Message {
            content: vec![ContentPart::Text { text: prompt }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: CommandId(mint("c_")),
            },
        };
        // The receiver is still here, so the send cannot fail.
        inbox.send(Delivery::Message(message)).unwrap_or(());
        drop(inbox);
        run(waiting)
    }

    /// Ends the door side once the loop has written `fiber_exited`: unlinks
    /// the socket, deletes the directory of a session that never got a
    /// prompt (`docs/invocation.md`, "Lifecycle"), drops `log`, the last
    /// handle on it, which releases the lock, and waits for stdout to have
    /// every line.
    pub fn close(self, log: Arc<Log>) {
        let Self {
            dir,
            socket,
            listener,
            printer,
        } = self;
        drop(listener);
        remove_socket(&socket);
        if !prompted(&dir) {
            fs::remove_dir_all(&dir).unwrap_or(());
        }
        drop(log);
        printer.join().unwrap_or(());
    }
}

/// Whether the session's log has a `turn_started`. A log that cannot be read
/// is kept, so nothing is deleted on a guess.
fn prompted(dir: &Path) -> bool {
    log::read(dir).map_or(true, |lines| {
        lines
            .iter()
            .any(|line| matches!(Event::from_envelope(line), Ok(Some(Event::TurnStarted(_)))))
    })
}

/// Binds the session's socket at `run/<session_id>`, mode 0600 in a 0700
/// directory. The session's lock holder owns the socket, so a stale one left
/// by a dead process is removed first.
fn bind(home: &Path, dir: &Path) -> Result<(PathBuf, UnixListener), Failure> {
    let run = home.join("run");
    let socket = run.join(dir.file_name().unwrap_or_default());
    if socket.as_os_str().len() > SOCKET_PATH_MAX {
        return Err(failure(
            ErrorCode::Usage,
            format!(
                "FIBER_HOME is too long: a session's socket path must fit in \
                 {SOCKET_PATH_MAX} bytes. Set FIBER_HOME to a shorter path."
            ),
        ));
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&run)
        .map_err(|e| io_failed(&run, &e))?;
    remove_socket(&socket);
    let listener = UnixListener::bind(&socket).map_err(|e| io_failed(&socket, &e))?;
    if let Err(e) = fs::set_permissions(&socket, Permissions::from_mode(0o600)) {
        remove_socket(&socket);
        return Err(io_failed(&socket, &e));
    }
    Ok((socket, listener))
}

fn remove_socket(socket: &Path) {
    // Nothing there is the usual case.
    fs::remove_file(socket).unwrap_or(());
}

fn io_failed(path: &Path, e: &std::io::Error) -> Failure {
    failure(ErrorCode::IoFailed, format!("{}: {e}", path.display()))
}

/// Copies every event to `out`, one JSON line each, until `fiber_exited` or
/// the end of the log. Durable lines serialize as the log wrote them, so
/// stdout filtered to them is `events.jsonl` byte for byte
/// (`docs/invocation.md`, "What a caller gets back").
fn print(mut watcher: Watcher, mut out: Box<dyn Write + Send>) {
    while let Ok(Some(line)) = watcher.recv() {
        let Ok(mut bytes) = serde_json::to_vec(&line) else {
            continue;
        };
        bytes.push(b'\n');
        // A reader that went away stops the copy, never the session.
        if out.write_all(&bytes).and_then(|()| out.flush()).is_err() {
            return;
        }
        if line.kind == "fiber_exited" {
            return;
        }
    }
}
