//! A driver `shell` (`docs/architecture.md`, "One inbox"). The
//! connection's reader checks it and registers its cancel, then runs it
//! on a thread of its own and reads on, so a `cancel` on the same connection
//! reaches it. With `send` false the thread answers the command itself,
//! and its output so far goes to the sending connection as
//! `shell_command_delta`; with `send` true the loop writes
//! `shell_command_started` before the command starts, every attached client
//! gets its output as `shell_command_delta` while it runs, and the thread
//! sends the shaped result to the loop's inbox, which writes it as
//! `shell_command` (`docs/invocation.md`, "Shell"; `docs/events.md`,
//! "`shell_command_started`"). The loop never runs it.

use contract::clock::Wake;
use contract::commands::Shell;
use contract::emit::Emit;
use contract::events::{
    CommandResult, Event, Progress, ShellCommand, ShellCommandDelta, ShellCommandStarted,
};
use contract::inbox::{Ack, Delivery, Handshake, Rejection};
use contract::shapes::{ContentPart, Process};
use contract::tool::{Bound, Output, Tool};
use contract::{CommandId, ErrorCode};
use log::{cut_joined, cut_notice};
use serde_json::{Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use support::cancel::{Cancel as _, Token};
use support::lock;

use crate::client::level::Outbox;
use crate::mint;
use crate::session::{Gate, envelope};

pub(crate) const COULD_NOT_START: &str = "The command could not start.";
/// What a `shell` stopped before its command started says when `cancel`
/// stopped it: the tool's own words (`tool_util.rs`, `cancelled_before`).
const CANCELLED_BEFORE_START: &str = "Cancelled before it started.";

/// A driver shell that passed its checks, with its cancel registered.
pub(crate) struct Running {
    tool: Arc<dyn Tool>,
    cancel: Arc<Token>,
    arguments: Map<String, Value>,
    stream: Stream,
}

/// Wakes a `send` run's wait for its start line when the run's token is
/// cancelled.
struct CancelWake(mpsc::Sender<()>);

impl Wake for CancelWake {
    fn wake(&self) {
        let _gone = self.0.send(());
    }
}

/// The emitter a driver shell runs under (`Emit`): it holds the command
/// until its start line is written, and turns the tool's paced output into
/// `shell_command_delta` lines.
struct Stream {
    gate: Arc<Gate>,
    command_id: CommandId,
    command: String,
    send: bool,
    /// The sending connection's route to its writer, when it has one:
    /// where a `send` false run's deltas go.
    outbox: Option<Outbox>,
    cancel: Arc<Token>,
    /// Shared with the `Delivery::ShellStarted` the loop takes: only the
    /// loop sets `Written`, while it writes the line; only this side sets
    /// `Abandoned`, and only from `Pending`.
    handshake: Arc<Mutex<Handshake>>,
    /// Whether a start line was written for this run, so its end is owed.
    started: AtomicBool,
}

impl Emit for Stream {
    fn emit(&self, event: &Event) {
        let Event::ToolCallDelta(Progress {
            text: Some(text), ..
        }) = event
        else {
            return;
        };
        let delta = Event::ShellCommandDelta(ShellCommandDelta {
            command_id: self.command_id.clone(),
            text: text.clone(),
        });
        if self.send {
            // Every attached client, through the log's watchers. The
            // command started only after its start line was written, so
            // no delta precedes it on any watcher.
            if let Some(log) = self.gate.log.upgrade() {
                log.emit(&delta);
            }
        } else if let Some(outbox) = &self.outbox {
            // The sending connection only, droppable, never logged.
            outbox.push(envelope(
                &self.gate.session_id,
                self.gate.clock.as_ref(),
                &delta,
            ));
        }
    }

    fn paced(&self) -> bool {
        true
    }

    /// With `send` false the command starts at once. With `send` true the
    /// loop must write `shell_command_started` first: this asks it to, and
    /// waits for the first of its answer, the run's cancel and the dropped
    /// acknowledgement. A start still `Pending` at a wake is abandoned and
    /// the command never starts; one already `Written` starts, and the
    /// tool's own cancel handling stops it.
    fn starting(&self) -> bool {
        if !self.send {
            return true;
        }
        let (wakes, woken) = mpsc::channel();
        let waker: Arc<dyn Wake> = Arc::new(CancelWake(wakes.clone()));
        self.cancel.subscribe(Arc::downgrade(&waker));
        let answers = wakes.clone();
        // The answer itself is read from the handshake: the loop sets it
        // before it answers, and a dropped acknowledgement leaves it
        // `Pending`.
        let ack = crate::client::guard(move |_answer| {
            let _gone = answers.send(());
        });
        self.gate.deliver(Delivery::ShellStarted(
            ShellCommandStarted {
                command_id: self.command_id.clone(),
                command: self.command.clone(),
            },
            Arc::clone(&self.handshake),
            ack,
        ));
        // A cancel before the subscription is not replayed.
        if !self.cancel.is_cancelled() {
            // Both senders live in this function, so the wait ends only
            // with a wake.
            let _woken = woken.recv();
        }
        drop(waker);
        let mut state = lock(&self.handshake);
        if *state == Handshake::Pending {
            *state = Handshake::Abandoned;
        }
        let written = *state == Handshake::Written;
        self.started.store(written, Ordering::SeqCst);
        written
    }
}

/// Checks `command` and registers its cancel with `gate`, so a `cancel`
/// read after this returns reaches it. Called on the reader thread. `id` is
/// the command's id; `outbox` is the sender's route to its writer, where a
/// `send` false run's output goes as it runs.
pub(crate) fn start(
    gate: &Arc<Gate>,
    id: &CommandId,
    command: &Shell,
    outbox: Option<Outbox>,
) -> Option<Running> {
    let tool = gate.driver_shell()?;
    let cancel = Arc::new(Token::new());
    gate.shells.track(Arc::clone(&cancel));
    let mut arguments = Map::new();
    arguments.insert("command".to_owned(), Value::String(command.command.clone()));
    Some(Running {
        tool,
        stream: Stream {
            gate: Arc::clone(gate),
            command_id: id.clone(),
            command: command.command.clone(),
            send: command.send,
            outbox,
            cancel: Arc::clone(&cancel),
            handshake: Arc::new(Mutex::new(Handshake::Pending)),
            started: AtomicBool::new(false),
        },
        cancel,
        arguments,
    })
}

impl Running {
    /// Runs the tool, answers through `ack`, then unregisters the cancel:
    /// once [`Gate`] no longer lists the shell, its answer is queued. With
    /// `send` true the shaped result goes to the loop's inbox instead of
    /// the ack, and the loop answers it when it writes `shell_command`. A
    /// result that never ran, or whose output never saved, answers the ack
    /// directly either way; when its start line was written, the loop is
    /// also sent an end line that carries the rejection, so the start has
    /// its end. The registry lock is not held across [`Tool::run`].
    pub(crate) fn finish(self, gate: &Gate, ack: Ack) {
        let output = self
            .tool
            .run(&self.arguments, self.cancel.as_ref(), &self.stream);
        let process = output.process.clone();
        let answered = answered(gate, output, self.tool.bound());
        if self.stream.send {
            match answered {
                Ok(Some(CommandResult::Shell {
                    output,
                    artifact,
                    process,
                })) => {
                    let line = ShellCommand {
                        command_id: Some(self.stream.command_id.clone()),
                        command: self.stream.command.clone(),
                        output,
                        artifact,
                        process,
                    };
                    gate.deliver(Delivery::Shell(line, ack));
                }
                Err(rejection) if self.stream.started.load(Ordering::SeqCst) => {
                    (ack.0)(Err(rejection.clone()));
                    self.end_rejected(gate, &rejection, process);
                }
                answered => (ack.0)(answered),
            }
        } else {
            (ack.0)(answered);
        }
        // A panic aborts the process, so nothing later cancels this shell.
        gate.shells.untrack(&self.cancel);
    }

    /// Ends a run whose start line was written but whose answer was a
    /// rejection: a `shell_command` carrying `<code>: <message>` as its
    /// output and the run's process, or neither an exit code nor a signal
    /// when none ran. The sender was answered already, so this line's
    /// acknowledgement is ignored.
    fn end_rejected(&self, gate: &Gate, rejection: &Rejection, process: Option<Process>) {
        let line = ShellCommand {
            command_id: Some(self.stream.command_id.clone()),
            command: self.stream.command.clone(),
            output: format!("{}: {}", rejection.code.name(), rejection.message),
            artifact: None,
            process: process.unwrap_or(Process {
                exit_code: None,
                signal: None,
                timed_out: false,
            }),
        };
        gate.deliver(Delivery::Shell(line, Ack(Box::new(|_| {}))));
    }

    /// Unregisters a shell that never ran, after the reader answered it.
    pub(crate) fn abandon(self, gate: &Gate) {
        gate.shells.untrack(&self.cancel);
    }
}

fn no_process(error: Option<contract::shapes::Failure>) -> Rejection {
    let message = match error {
        Some(error) => error.message,
        None => COULD_NOT_START.to_owned(),
    };
    Rejection {
        code: ErrorCode::InvalidArguments,
        message,
    }
}

fn answered(gate: &Gate, output: Output, bound: Bound) -> contract::inbox::Answer {
    if let Some(spilled) = output.spilled {
        return answered_spilled(gate, spilled, output.process, output.error);
    }
    let Some(process) = output.process else {
        if output.error.is_none() {
            return Err(stopped_before_start(gate));
        }
        return Err(no_process(output.error));
    };
    let full = joined(&output.content);
    let (kept, artifact) = cut(gate, &full, bound);
    Ok(Some(CommandResult::Shell {
        output: kept,
        artifact,
        process,
    }))
}

/// A shell whose tool never started a process and reports no error: the
/// tool's `cancelled_before`, whose cancel token fired before `Tool::run`
/// (a real failure to start carries its error). Admission closes before
/// the shells are sealed on every `close` path, and never on `cancel`, so
/// a closed admission is a `close` stop, answered `closing` as a running
/// shell is, and an open one is a `cancel` stop, answered `cancelled`.
fn stopped_before_start(gate: &Gate) -> Rejection {
    if gate.admission.admit(|| ()).is_none() {
        return Rejection {
            code: ErrorCode::Closing,
            message: crate::client::ENDED.to_owned(),
        };
    }
    Rejection {
        code: ErrorCode::Cancelled,
        message: CANCELLED_BEFORE_START.to_owned(),
    }
}

/// A shell that already spilled its output past 1 MiB: the spilled file is
/// the artifact, shaped without a second write or cut. Text parts beside
/// the spill are ignored; the command's process is kept as it ended.
fn answered_spilled(
    gate: &Gate,
    spilled: contract::tool::Spilled,
    process: Option<contract::shapes::Process>,
    error: Option<contract::shapes::Failure>,
) -> contract::inbox::Answer {
    let contract::tool::Spilled {
        head,
        tail,
        removed,
        file,
        ..
    } = spilled;
    match file {
        Ok(kept) => {
            let Some(process) = process else {
                return Err(no_process(error));
            };
            let Some(log) = gate.log.upgrade() else {
                return Err(Rejection {
                    code: ErrorCode::Closing,
                    message: crate::client::ENDED.to_owned(),
                });
            };
            let artifact = spill_artifact(&log, &kept.path);
            let output = if removed == 0 {
                format!("{head}{tail}")
            } else {
                let notice = cut_notice(removed, Ok(&kept.path));
                cut_joined(&head, &notice, &tail)
            };
            Ok(Some(CommandResult::Shell {
                output,
                artifact: Some(artifact),
                process,
            }))
        }
        Err(unsaved) => {
            if let Some(left) = unsaved.left.as_ref()
                && let Err(remove) = std::fs::remove_file(left)
            {
                return Err(Rejection {
                    code: ErrorCode::IoFailed,
                    message: format!(
                        "The partial output at {} could not be removed: {remove}.",
                        left.display()
                    ),
                });
            }
            let Some(process) = process else {
                return Err(no_process(error));
            };
            let notice = cut_notice(removed, Err(unsaved.message.as_str()));
            let output = cut_joined(&head, &notice, &tail);
            Ok(Some(CommandResult::Shell {
                output,
                artifact: None,
                process,
            }))
        }
    }
}

/// A spilled file as the driver's artifact: its path relative to the
/// session directory, or absolute when it is not under it.
fn spill_artifact(log: &log::Log, path: &std::path::Path) -> String {
    match path.strip_prefix(log.dir()) {
        Ok(relative) => relative.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

fn joined(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Pdf(_) | ContentPart::Unknown => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn cut(gate: &Gate, full: &str, bound: Bound) -> (String, Option<String>) {
    let cap = bound.start.saturating_add(bound.end);
    if full.len() <= cap {
        return (full.to_owned(), None);
    }
    let Some(log) = gate.log.upgrade() else {
        return (full.to_owned(), None);
    };
    let name = format!("{}.txt", mint("o_"));
    log.cut_output(full, bound, &name)
}
