//! Steering a running session over its socket (`docs/delegates.md`,
//! "Talking to a delegate"): one connection per message. The parent is an
//! ordinary client of the delegate's socket (`docs/delegates.md`,
//! "Streams"; `docs/invocation.md`, "Processes").

use std::path::Path;

use contract::commands::{Command, CommandLine, ContentArgs, SentPart, SubscribeLevel};
use contract::shapes::Failure;
use contract::{CommandId, SessionId};

use crate::attach::{Unsubscribed, is_answer, next_line, open, rejected, send, subscribed};
use crate::mint;

/// A test-only pause point at the acknowledgement wait: armed by the
/// commit-order test, which holds the acknowledgement until this fires.
/// `None` outside that test, so the shipped path sends nothing extra and
/// waits on nothing extra. A hook, not an outside seam: nothing the test
/// can observe separates the subscribe write from its answer
/// (`docs/testing.md`, "Waits and timeouts"; the plan in #540).
#[cfg(test)]
pub(crate) struct AckWait {
    pub(crate) entered: std::sync::mpsc::Sender<()>,
    pub(crate) release: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
static ACK_WAIT: std::sync::Mutex<Option<AckWait>> = std::sync::Mutex::new(None);

/// Arms the acknowledgement-wait pause point for the next `steer` call.
/// One shot: the next call takes it, so later calls run unpaused.
#[cfg(test)]
pub(crate) fn hold_ack_wait_for_test(hold: AckWait) {
    if let Ok(mut armed) = ACK_WAIT.lock() {
        *armed = Some(hold);
    }
}

/// Signals the armed test, if any, and waits for its release: the
/// acknowledgement read below starts only after. No deadline of its own:
/// the test's release sender drops when the test ends, which releases
/// this too (`docs/testing.md`, "Waits and timeouts").
#[cfg(test)]
fn pause_at_ack_wait() {
    let Some(hold) = ACK_WAIT.lock().ok().and_then(|mut armed| armed.take()) else {
        return;
    };
    let _sent = hold.entered.send(());
    let _released = hold.release.recv();
}

/// A test-only read-completion signal: fired after the acknowledgement
/// read has returned and before `commit` runs, so the commit-order test
/// can assert inside `commit` that the answer was read. `None` outside
/// that test, so the shipped path sends nothing extra. A hook, not an
/// outside seam: nothing the test can observe separates the answer's
/// arrival from `commit` (`docs/testing.md`, "Waits and timeouts"; the
/// plan in #540).
#[cfg(test)]
static ACK_READ: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>> =
    std::sync::Mutex::new(None);

/// Arms the read-completion signal for the next `steer` call. One shot:
/// the next answered subscribe takes it, so later calls signal nothing.
#[cfg(test)]
pub(crate) fn hold_ack_read_for_test(read: std::sync::mpsc::Sender<()>) {
    if let Ok(mut armed) = ACK_READ.lock() {
        *armed = Some(read);
    }
}

/// Fires the armed read-completion signal, if any: the acknowledgement
/// read above has returned, and `commit` runs next.
#[cfg(test)]
fn note_ack_read() {
    let read = ACK_READ.lock().ok().and_then(|mut armed| armed.take());
    if let Some(read) = read {
        let _sent = read.send(());
    }
}

/// What one `steer` attempt ended in. An outcome, like [`Watched`], not an
/// error type.
#[derive(Debug, Clone, PartialEq)]
pub enum Steered {
    /// The session accepted the steering message.
    Accepted,
    /// The session rejected it, with its code and message.
    Rejected(Failure),
    /// The session is gone: no socket, a failed write, or the connection
    /// closed before the steer's answer.
    Gone,
    /// `commit` refused, so no byte of the `steer` line was written.
    Withheld,
}

/// Steers the session `id` running under `home` with `text`, on one
/// connection: `subscribe` at `summary`, which reads no log and is not
/// counted in `clients`, then the `steer` carrying `text` as one text part,
/// then a `tools` probe. `command` is the steer's id, the same on every
/// attempt of one call, so a resend after a lost answer is answered
/// `duplicate_command` instead of applied twice.
///
/// After the subscribe is accepted `commit` runs once: false ends as
/// [`Steered::Withheld`] with nothing written, true writes the `steer`
/// line. No byte of it is written unless `commit` returned true on that
/// connection, and `steer` holds no lock of the caller's across the write.
/// `on_queued` runs at most once, when the session's reader queued the
/// message: the probe's `command_accepted` proves it when it arrives
/// first, and the steer's own at-once `command_accepted` proves it
/// otherwise, since the reader queues, emits `steering_queue` and only
/// then answers.
pub fn steer(
    home: &Path,
    id: &SessionId,
    command: &str,
    text: &str,
    commit: &mut dyn FnMut() -> bool,
    on_queued: &mut dyn FnMut(),
) -> Steered {
    let socket = home.join("run").join(&id.0);
    let opened = match open(&socket, id, SubscribeLevel::Summary) {
        Ok(opened) => opened,
        Err(_) => return Steered::Gone,
    };
    // Test-only: the commit-order test holds here until it holds the
    // acknowledgement, so a commit before the answer fails deterministically.
    #[cfg(test)]
    pause_at_ack_wait();
    let (mut reader, mut writer) = match subscribed(opened, id) {
        Ok(pair) => pair,
        Err(Unsubscribed::Rejected(error) | Unsubscribed::Version(error)) => {
            return Steered::Rejected(Failure::from(error));
        }
        Err(_) => return Steered::Gone,
    };
    // Test-only: the commit-order test asserts inside `commit` that this
    // fired, so a commit before the answer fails deterministically.
    #[cfg(test)]
    note_ack_read();
    if !commit() {
        return Steered::Withheld;
    }
    if send(
        &mut writer,
        id,
        &CommandLine {
            id: CommandId(command.to_owned()),
            command: Command::Steer(ContentArgs {
                content: vec![SentPart::Text {
                    text: text.to_owned(),
                }],
            }),
        },
    )
    .is_err()
    {
        return Steered::Gone;
    }
    let probe = mint("c_");
    if send(
        &mut writer,
        id,
        &CommandLine {
            id: CommandId(probe.clone()),
            command: Command::Tools,
        },
    )
    .is_err()
    {
        return Steered::Gone;
    }
    let mut queued = false;
    loop {
        let Some((_, line)) = next_line(&mut reader) else {
            return Steered::Gone;
        };
        if is_answer(&line, "command_rejected", command) {
            return Steered::Rejected(Failure::from(rejected(&line)));
        }
        if is_answer(&line, "command_accepted", command) {
            // The at-once answer is itself the queue proof: the reader
            // queues, emits and only then answers, so the probe may
            // never beat it.
            if !queued {
                on_queued();
            }
            return Steered::Accepted;
        }
        if !queued && is_answer(&line, "command_accepted", &probe) {
            queued = true;
            on_queued();
        }
    }
}

#[cfg(test)]
#[path = "steer_tests.rs"]
mod tests;
