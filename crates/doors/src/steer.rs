//! Steering a running session over its socket (`docs/delegates.md`,
//! "Talking to a delegate"): one connection per message. The parent is an
//! ordinary client of the delegate's socket (`docs/delegates.md`,
//! "Streams"; `docs/invocation.md`, "Processes").

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;

use contract::SessionId;
use contract::shapes::Failure;

use crate::attach::{check_version, is_answer, next_line, rejection, send};
use crate::mint;

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
/// `on_queued` runs at most once, when the probe's `command_accepted`
/// arrives before the steer's answer: the session's reader answers the
/// probe once it handed the steer to its inbox, so the probe proves the
/// message is queued.
pub fn steer(
    home: &Path,
    id: &SessionId,
    command: &str,
    text: &str,
    commit: &mut dyn FnMut() -> bool,
    on_queued: &mut dyn FnMut(),
) -> Steered {
    let socket = home.join("run").join(&id.0);
    let Ok(stream) = UnixStream::connect(&socket) else {
        return Steered::Gone;
    };
    let Ok(clone) = stream.try_clone() else {
        return Steered::Gone;
    };
    let mut reader = BufReader::new(clone);
    let mut writer = stream;
    let sub = mint("c_");
    if send(
        &mut writer,
        id,
        &sub,
        "subscribe",
        serde_json::json!({"level": "summary"}),
    )
    .is_err()
    {
        return Steered::Gone;
    }
    // The acknowledgement is written before the writer starts, so it is
    // the first line this connection reads, ahead of the fold.
    loop {
        let Some((_, line)) = next_line(&mut reader) else {
            return Steered::Gone;
        };
        if is_answer(&line, "command_rejected", &sub) {
            return Steered::Rejected(rejection(&line));
        }
        if is_answer(&line, "command_accepted", &sub) {
            if let Err(failure) = check_version(id, &line) {
                return Steered::Rejected(failure);
            }
            break;
        }
    }
    if !commit() {
        return Steered::Withheld;
    }
    if send(
        &mut writer,
        id,
        command,
        "steer",
        serde_json::json!({"content": [{"type": "text", "text": text}]}),
    )
    .is_err()
    {
        return Steered::Gone;
    }
    let probe = mint("c_");
    if send(&mut writer, id, &probe, "tools", serde_json::json!({})).is_err() {
        return Steered::Gone;
    }
    let mut queued = false;
    loop {
        let Some((_, line)) = next_line(&mut reader) else {
            return Steered::Gone;
        };
        if is_answer(&line, "command_rejected", command) {
            return Steered::Rejected(rejection(&line));
        }
        if is_answer(&line, "command_accepted", command) {
            return Steered::Accepted;
        }
        if !queued && is_answer(&line, "command_accepted", &probe) {
            queued = true;
            on_queued();
        }
    }
}
