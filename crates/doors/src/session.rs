//! One session process's boundary (`docs/invocation.md`, "Lifecycle" and
//! "Processes"): its socket at `run/<session_id>` in Fiber home, its event
//! stream on stdout, `fiber_started` first and `fiber_exited` last.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, Permissions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use contract::events::{
    Event, FiberExited, FiberStarted, FinalMessage, MessageOutcome, TurnOutcome,
};
use contract::shapes::{Failure, Question, Tokens, Usage};
use contract::{Envelope, ErrorCode, SessionId};
use log::{Log, Watcher};

use crate::{failure, mint};

/// The longest socket path the platform binds: `sun_path` less its
/// terminating byte (`docs/state.md`, "Sockets").
const SOCKET_PATH_MAX: usize = if cfg!(target_os = "macos") { 103 } else { 107 };

/// A running session process: its log, its socket and the thread copying
/// its events to stdout.
pub struct Session {
    log: Arc<Log>,
    dir: PathBuf,
    socket: PathBuf,
    listener: UnixListener,
    printer: JoinHandle<()>,
}

impl Session {
    /// Starts a new session in `sessions` (`log::sessions_dir`): creates its
    /// directory, binds its socket in `home`'s `run/`, starts copying every
    /// event to `out` as one JSON line each, and writes `fiber_started`. A
    /// failure here leaves nothing behind and is a failure before any session
    /// exists.
    pub fn start(
        home: &Path,
        sessions: &Path,
        out: Box<dyn Write + Send>,
    ) -> Result<Self, Failure> {
        let id = SessionId(mint("s_"));
        let run = home.join("run");
        let socket = run.join(&id.0);
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
        let log = Arc::new(
            Log::create(sessions, id.clone()).map_err(|e| failure(e.code(), e.to_string()))?,
        );
        let dir = sessions.join(&id.0);
        let listener = match bind(&socket) {
            Ok(listener) => listener,
            Err(e) => {
                fs::remove_dir_all(&dir).unwrap_or(());
                return Err(e);
            }
        };
        let watcher = log.watch();
        let printer = thread::Builder::new()
            .name("stdout".to_owned())
            .spawn(move || print(watcher, out));
        let printer = match printer {
            Ok(printer) => printer,
            Err(e) => {
                remove_socket(&socket);
                fs::remove_dir_all(&dir).unwrap_or(());
                return Err(failure(
                    ErrorCode::IoFailed,
                    format!("cannot start a thread: {e}"),
                ));
            }
        };
        let session = Self {
            log,
            dir,
            socket,
            listener,
            printer,
        };
        let started = Event::FiberStarted(FiberStarted {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            resumed: false,
        });
        if let Err(e) = session.log.append(&started, None, None) {
            let error = failure(e.code(), e.to_string());
            session.close(false);
            return Err(error);
        }
        Ok(session)
    }

    /// The session's log, for the loop.
    pub fn log(&self) -> Arc<Log> {
        Arc::clone(&self.log)
    }

    /// Ends the session process once the loop has stopped and dropped its
    /// log: writes `fiber_exited` with the final message, the usage and
    /// `ran`'s error or the last turn's, unlinks the socket and releases the
    /// lock. A session whose log has no `turn_started` deletes its directory
    /// (`docs/invocation.md`, "Lifecycle"). Returns the exit code: 1 when
    /// `ran` failed or the last turn did (`docs/errors.md`, "What a caller
    /// gets"), otherwise 0.
    pub fn exit(self, ran: Result<(), Failure>) -> i32 {
        let folded = log::read(&self.dir)
            .map_err(|e| failure(e.code(), e.to_string()))
            .and_then(|lines| {
                fold(&lines).map_err(|e| {
                    failure(
                        ErrorCode::LogCorrupt,
                        format!("a log line does not read as its kind: {e}"),
                    )
                })
            });
        let (fold, read_error) = match folded {
            Ok(fold) => (fold, None),
            Err(e) => (Fold::default(), Some(e)),
        };
        let error = ran.err().or(read_error).or(fold.error);
        let exit_code = i32::from(error.is_some());
        let exited = Event::FiberExited(FiberExited {
            exit_code,
            usage: fold.usage.into(),
            final_message: fold.final_message,
            error,
            suspended_on: None,
            questions: fold.questions,
        });
        // With no `fiber_exited` the log reads as a process that died, which
        // is what it is.
        let written = self.log.append(&exited, None, None).is_ok();
        self.close(fold.prompted);
        if written { exit_code } else { 1 }
    }

    /// Unlinks the socket, deletes the directory of a session that never got
    /// a prompt, releases the lock and waits for stdout to have every line.
    fn close(self, prompted: bool) {
        let Self {
            log,
            dir,
            socket,
            listener,
            printer,
        } = self;
        drop(listener);
        remove_socket(&socket);
        if !prompted {
            fs::remove_dir_all(&dir).unwrap_or(());
        }
        // The last handle on the log: dropping it releases the lock and ends
        // the printer's watcher.
        drop(log);
        printer.join().unwrap_or(());
    }
}

/// Binds the session's socket, mode 0600. The lock's holder owns the socket,
/// so a stale one left by a dead process is removed first.
fn bind(socket: &Path) -> Result<UnixListener, Failure> {
    remove_socket(socket);
    let listener = UnixListener::bind(socket).map_err(|e| io_failed(socket, &e))?;
    if let Err(e) = fs::set_permissions(socket, Permissions::from_mode(0o600)) {
        remove_socket(socket);
        return Err(io_failed(socket, &e));
    }
    Ok(listener)
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

/// What `fiber_exited` reports, folded from the session's log.
#[derive(Default)]
struct Fold {
    prompted: bool,
    final_message: Option<FinalMessage>,
    error: Option<Failure>,
    questions: Option<Vec<Question>>,
    usage: Totals,
}

/// Usage totals while folding: billed calls and their known costs.
struct Totals {
    tokens: Tokens,
    billed: usize,
    cost: Option<f64>,
    subscription_cost: f64,
}

impl Default for Totals {
    fn default() -> Self {
        Self {
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 0,
            },
            billed: 0,
            cost: None,
            subscription_cost: 0.0,
        }
    }
}

impl From<Totals> for Usage {
    fn from(t: Totals) -> Self {
        Self {
            tokens: t.tokens,
            // `docs/events.md`, "usage": 0 with no billed call, null when
            // no billed call had a known cost.
            cost: if t.billed == 0 { Some(0.0) } else { t.cost },
            subscription_cost: t.subscription_cost,
        }
    }
}

fn fold(lines: &[Envelope]) -> Result<Fold, serde_json::Error> {
    let mut fold = Fold::default();
    for line in lines {
        let event = Event::from_envelope(line)?;
        if let Some(Event::TurnStarted(_)) = &event {
            fold.prompted = true;
            fold.final_message = None;
            fold.error = None;
            fold.questions = None;
        } else if let Some(Event::AssistantMessageCompleted(message)) = &event
            && message.outcome == MessageOutcome::Completed
            && let Some(id) = &line.action_id
        {
            fold.final_message = Some(FinalMessage {
                final_action_id: id.clone(),
                text: message.text.clone(),
            });
        } else if let Some(Event::TurnCompleted(turn)) = &event {
            if turn.outcome == TurnOutcome::Failed {
                fold.final_message = None;
            }
            fold.error = turn.error.clone();
            fold.questions = turn.questions.clone();
        } else if let Some(Event::UsageRecorded(call)) = &event {
            let usage = &mut fold.usage;
            let tokens = &mut usage.tokens;
            tokens.input += call.tokens.input;
            tokens.cache_read += call.tokens.cache_read;
            tokens.output += call.tokens.output;
            for (lifetime, n) in &call.tokens.cache_write {
                *tokens.cache_write.entry(lifetime.clone()).or_default() += n;
            }
            if call.subscription == Some(true) {
                usage.subscription_cost += call.cost.unwrap_or(0.0);
            } else {
                usage.billed += 1;
                if let Some(cost) = call.cost {
                    *usage.cost.get_or_insert(0.0) += cost;
                }
            }
        }
    }
    Ok(fold)
}
