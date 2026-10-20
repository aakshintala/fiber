//! A driver `propose_notes` (`docs/invocation.md`, "What each command
//! does"): gathers evidence on a thread of its own and delivers it to the
//! loop's inbox, so this reader reads on.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::thread;

use contract::inbox::{Ack, Delivery};
use contract::notes::{Evidence, NotesAsk, ProposeNotesArgs, SessionTally};
use contract::{CommandId, ErrorCode};

use crate::client::Conn;

/// The command, for the answer when no workspace is wired.
const COMMAND: &str = "propose_notes";

/// At most one session item per prefix.
const SESSION_CAP: usize = 64;

/// Takes `propose_notes`: reads the wired workspace, answering
/// `unknown_command` when none is wired, then gathers on a thread of its
/// own and delivers to the loop's inbox. A thread that cannot start
/// rejects `io_failed`; a send after the thread died answers `closing`
/// through the dropped acknowledgement.
pub(crate) fn run(conn: &mut Conn, id: CommandId, args: ProposeNotesArgs) {
    let Some(workspace) = conn.gate.notes_workspace() else {
        crate::client::reject(
            conn,
            Some(id),
            ErrorCode::UnknownCommand,
            &crate::client::not_built(COMMAND),
        );
        return;
    };
    let gate = Arc::clone(&conn.gate);
    let home = gate.home.clone();
    let dir = gate.dir.clone();
    let (tx, rx) = mpsc::channel::<(ProposeNotesArgs, Ack)>();
    let spawned = thread::Builder::new()
        .name("notes".to_owned())
        .spawn(move || {
            if let Ok((args, ack)) = rx.recv() {
                let evidence = gather(&home, &dir, &workspace);
                gate.deliver(Delivery::ProposeNotes(NotesAsk { args, evidence }, ack));
            }
        });
    if spawned.is_err() {
        crate::client::reject(
            conn,
            Some(id),
            ErrorCode::IoFailed,
            crate::shell::COULD_NOT_START,
        );
        return;
    }
    let ack = crate::client::inbox_ack(conn, id);
    // The thread waits in `recv`, so the send fails only if it died first:
    // the dropped acknowledgement answers `closing`.
    if let Err(mpsc::SendError((_args, ack))) = tx.send((args, ack)) {
        drop(ack);
    }
}

/// Gathers `propose_notes` evidence for one topic: the session counts.
/// The loop appends the person's answer last.
pub(crate) fn gather(home: &Path, dir: &Path, workspace: &Path) -> Vec<Evidence> {
    session_evidence(home, dir, workspace)
}

/// One item per counted prefix: `<count> escalations answered allow for
/// `<prefix>``, sorted by count descending, then prefix ascending, at most
/// [`SESSION_CAP`]. `here` counts the escalations whose recorded identity
/// resolves to this session's project; each distinct identity resolves
/// once. An escalation from another `projects/<key>` directory is another
/// project's without running `git`; one with no identity, or whose
/// identity no longer resolves, counts there too.
fn session_evidence(home: &Path, dir: &Path, workspace: &Path) -> Vec<Evidence> {
    let mut tallies: HashMap<String, (usize, usize)> = HashMap::new();
    let here = crate::project(workspace);
    let current = current_key(dir);
    let mut same: HashMap<String, bool> = HashMap::new();
    for escalation in log::allowed_escalations(home) {
        let tally = tallies.entry(escalation.prefix.clone()).or_insert((0, 0));
        tally.0 += 1;
        let identity = escalation.identity.as_deref();
        let is_here = identity.is_some_and(|identity| {
            current.as_deref() == Some(escalation.project.as_str())
                && *same
                    .entry(identity.to_owned())
                    .or_insert_with(|| crate::project(Path::new(identity)) == here)
        });
        if is_here {
            tally.1 += 1;
        }
    }
    let mut prefixes: Vec<(String, usize, usize)> = tallies
        .into_iter()
        .map(|(prefix, (count, here))| (prefix, count, here))
        .collect();
    prefixes.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    prefixes.truncate(SESSION_CAP);
    prefixes
        .into_iter()
        .filter_map(|(prefix, count, here)| {
            let count = u32::try_from(count).ok()?;
            let here = u32::try_from(here).ok()?;
            SessionTally::new(prefix, count, here)
        })
        .map(Evidence::Session)
        .collect()
}

/// The `projects/<key>` directory name holding the session in `dir`:
/// `None` when `dir` is not laid out as one.
fn current_key(dir: &Path) -> Option<String> {
    dir.parent()?
        .parent()?
        .file_name()
        .map(|key| key.to_string_lossy().into_owned())
}

#[cfg(test)]
#[path = "notes_tests.rs"]
mod tests;
