//! Owns the ids of the commands this session accepted, so a repeated id is
//! rejected `duplicate_command`, and the ids each subscribed connection's
//! reader has read and the writer has not yet answered.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::thread::{self, ThreadId};

use super::lock;

use contract::CommandId;

/// The id of every command this process accepted or is running, across
/// connections, so a repeat is rejected `duplicate_command`. A session
/// "remembers the id of every command it accepted for as long as its
/// process runs" (`docs/invocation.md`, "The command line"), so the set
/// stays unbounded: one `String` per accepted command for the process's life.
pub(crate) struct Accepted {
    ids: Mutex<HashSet<String>>,
    /// The lines each tracked reader thread has read, for the writer that
    /// answers them. A leaf lock: nothing is called while it is held.
    reads: Mutex<HashMap<ThreadId, Reads>>,
}

/// What one tracked reader has read.
#[derive(Default)]
struct Reads {
    /// The ids read and not yet answered, in read order. A repeated id
    /// appears once for each time it was read.
    open: Vec<String>,
    /// The ids the close grace cut: each was answered `indeterminate`, so
    /// its own answer is never written.
    cut: HashSet<String>,
}

/// Whether the writer writes an acknowledgement it holds.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Answered {
    Write,
    /// The command's id was cut and already answered `indeterminate`.
    Skip,
}

impl Accepted {
    pub(crate) fn new() -> Self {
        Self {
            ids: Mutex::new(HashSet::new()),
            reads: Mutex::new(HashMap::new()),
        }
    }

    /// Claims `id` before its command is dispatched. False when an earlier
    /// command holds it, running or accepted. While the calling thread is
    /// tracked, `id` is also recorded as read and unanswered, whatever the
    /// result: a duplicate is answered too.
    pub(crate) fn reserve(&self, id: &CommandId) -> bool {
        let fresh = lock(&self.ids).insert(id.0.clone());
        if let Some(reads) = lock(&self.reads).get_mut(&thread::current().id()) {
            reads.open.push(id.0.clone());
        }
        fresh
    }

    /// Frees `id` once its command is rejected, so a client may retry it.
    pub(crate) fn release(&self, id: &CommandId) {
        lock(&self.ids).remove(&id.0);
    }

    /// Starts recording the lines `reader` reads.
    pub(crate) fn track(&self, reader: ThreadId) {
        lock(&self.reads).entry(reader).or_default();
    }

    /// Stops recording `reader`'s lines and forgets what it held.
    pub(crate) fn untrack(&self, reader: ThreadId) {
        lock(&self.reads).remove(&reader);
    }

    /// The writer is about to write the acknowledgement of command `id`:
    /// marks it answered. `code` is the rejection's code, `None` for an
    /// acceptance. A `malformed` rejection counts for nothing: `on_line`
    /// rejects a line it cannot classify before it reserves, so the id it
    /// names may belong to a command still running. A cut id is skipped
    /// once, so its command gets exactly one answer.
    pub(crate) fn answered(&self, reader: ThreadId, id: &str, code: Option<&str>) -> Answered {
        if code == Some("malformed") {
            return Answered::Write;
        }
        let mut reads = lock(&self.reads);
        let Some(reads) = reads.get_mut(&reader) else {
            return Answered::Write;
        };
        if reads.cut.remove(id) {
            return Answered::Skip;
        }
        if let Some(at) = reads.open.iter().position(|open| open == id) {
            reads.open.remove(at);
        }
        Answered::Write
    }

    /// How many lines `reader` has read that no acknowledgement has
    /// answered yet.
    pub(crate) fn unanswered(&self, reader: ThreadId) -> usize {
        lock(&self.reads)
            .get(&reader)
            .map_or(0, |reads| reads.open.len())
    }

    /// Takes every unanswered id of `reader`, in read order, and remembers
    /// each as cut so that its own answer, if it comes, is skipped.
    pub(crate) fn cut(&self, reader: ThreadId) -> Vec<String> {
        let mut reads = lock(&self.reads);
        let Some(reads) = reads.get_mut(&reader) else {
            return Vec::new();
        };
        let open = std::mem::take(&mut reads.open);
        reads.cut.extend(open.iter().cloned());
        open
    }
}
