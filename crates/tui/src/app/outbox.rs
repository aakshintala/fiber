//! The commands waiting for their answer: the link they go out on, the
//! drafts they carry, the lines held until the hub connects, and the
//! written lines a reconnect sends again (`docs/tui.md`, "A dropped
//! connection"; `docs/invocation.md`, "The command line").
//!
//! The outbox owns only this correlation state: applying a failure stays
//! with `App`, which owns the queue, the offer, the phase and the draft.

use std::collections::HashMap;

use contract::commands::hub::{HubCommand, HubCommandLine};
use contract::commands::{Command, CommandLine, HistoryArgs, ImportSource, SubscribeLevel};
use contract::{CommandId, Seq, SessionId};
use serde_json::Value;

use super::Sent;
use crate::input::Draft;

/// What the terminal is attached to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Phase {
    /// No session yet.
    Starting,
    /// `start` sent, or held until the hub connects; waiting for its answer.
    Pending {
        /// The `start` command's id.
        command_id: String,
    },
    /// Attached to a session.
    Attached {
        /// The session.
        session: SessionId,
        /// Whether a turn is running.
        busy: bool,
    },
}

/// Which command the terminal sent and waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Start,
    Prompt,
    Steer,
    Cancel,
    Reply,
    SteerDrop,
    Shell,
    /// A built-in command such as `compact`, `reload` or `close`.
    Command,
    /// An `import` for a named source or both (`None` for a bare
    /// `/import`); its answer's report is titled from it.
    Import(Option<ImportSource>),
}

/// The feature waiting for a command's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Waiting {
    /// A rail card's dismiss, carrying the target session.
    Dismiss(SessionId),
    Fork,
    RewindHistory,
    RewindSend,
    ChangeWorkspace(SessionId),
    ClearStart,
    ClearClose,
    History,
    /// A viewer's `read_file` fetch of `path` in `session`; `id` is the
    /// viewer's request number.
    ImageFetch {
        id: u32,
        session: String,
        path: String,
    },
    /// A display `read_file` fetch of `path` in `session`.
    ImageDisplay {
        session: String,
        path: String,
    },
    Sessions,
    Abandoned,
    /// A workspace picker `directories` listing for the directory text as
    /// sent, such as `/a/`.
    Directories(String),
    /// A home `subscribe` for a session's level.
    Subscribe {
        session: SessionId,
        level: SubscribeLevel,
    },
    /// A `close` with `now` stopping a live row.
    Stop,
    /// A `delete` for an exited row.
    Delete,
    /// The first-run offer's `import`.
    OfferImport,
    /// `/delete`'s discovery dry run, for the command's generation.
    DeleteDiscovery(u64),
    /// `/delete`'s confirmed stop of one session, for the command's
    /// generation.
    DeleteStop {
        generation: u64,
        session: SessionId,
    },
    /// `/delete`'s exact-set delete, for the command's generation.
    DeleteFinal(u64),
    /// `/archive`'s discovery dry run, for the command's generation.
    ArchiveDiscovery(u64),
    /// `/archive`'s confirmed stop of one session, for the command's
    /// generation.
    ArchiveStop {
        generation: u64,
        session: SessionId,
    },
    /// `/archive`'s one-session archive, for the command's generation.
    ArchiveFinal(u64),
    /// Home's `feed`.
    Feed,
    /// Home's `recent` pages, scoped or not.
    Recent(RecentAsk),
    /// The quit question's closes.
    Close,
    /// A Changed files `figures` numstat query.
    Figures,
    /// A Changed files `diff` query.
    Diff,
    /// A hooks view `hooks` query.
    Hooks,
    /// The panel's branch query.
    Branch,
    /// The panel's `quota` query or the quota view's fetch.
    Quota,
    /// A call view's `artifact_page` fetch.
    ArtifactPage,
    /// A configuration view's `tools`, `skills` or `mcp_servers` ask,
    /// or a per-server driver command for the open MCP view.
    ConfigAsk,
    /// A `credentials` labels query.
    Credentials,
    /// A `credential` switch saving its label.
    CredentialSwitch,
    /// An item view's `job_stop`.
    JobStop,
    /// An item view's `job_input`.
    JobInput,
    /// A conversation search's `history` fetch; `find_answered` settles it.
    Find,
    /// A session's `commands` ask filling the `/` list; `commands_answered`
    /// settles it where it clears `commands_id`.
    Commands,
    /// A `model` command from the picker; its accept or refusal settles it.
    Model,
    /// A `reply` command from the question form or the offer; `reply_ack`
    /// settles it.
    Reply,
    /// An `offered_file` command reading the held offer's source;
    /// `file_answered` settles it, and every refusal but `DuplicateCommand`.
    OfferFile,
    /// A `steer` command from the steering queue or a delegate's box;
    /// its answer settles generically, with no handler of its own.
    Steer,
    /// A `!` command's `shell` run; its accept settles it, then the fold.
    Shell,
}

/// Which `recent` ask a [`Waiting::Recent`] tag stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecentAsk {
    /// The unscoped first page.
    First,
    /// A later unscoped page.
    Page,
    /// A page scoped to one project.
    Scoped(String),
    /// An archived page scoped to one project.
    Archived(String),
}

/// The hub connection, as the terminal sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Link {
    /// Not connected yet: an Enter is held until `hub_hello`.
    #[default]
    Waiting,
    /// The hub spoke a `hub_hello` this terminal reads.
    Up,
    /// The hub could not be reached or hung up; the terminal retries with
    /// backoff (`docs/tui.md`, "A dropped connection").
    Down,
    /// The hub runs a schema this terminal cannot read: never retried.
    Refused,
}

/// One session command written with no answer yet.
#[derive(Debug, Clone)]
pub(crate) struct Kept {
    /// The command's id.
    pub(crate) id: String,
    /// The session it names.
    pub(crate) session: SessionId,
    /// The line as written, sent again unchanged.
    pub(crate) line: String,
}

/// The link, the commands with no answer yet, the lines held until the
/// hub connects, and the written lines a reconnect sends again.
/// `pending` holds every command made through the outbox with no answer
/// yet; `held` holds the made-but-unwritten subset; `kept` holds
/// written-but-unanswered session commands except `subscribe` and
/// `commands`, which a reconnect sends afresh. Kept ids are always
/// pending keys: `start` carries no `session_id`, so it is never kept.
/// `starts` holds every constructed `start` line with no answer yet, by
/// command id: the hooks install offer's retry resends them unchanged.
/// `hub_sent` pairs every fold-cruising hub line with the id it went
/// out under, so a failed write settles by the carried id instead of
/// parsing it back out of the line; an entry lives until the command
/// settles, purged with its wait or with its pending entry, whichever
/// goes first.
#[derive(Debug, Default)]
pub(crate) struct Outbox {
    /// Feature owners, independent of generic draft settlement, in the
    /// order their commands went out; no id appears twice.
    waits: Vec<(String, Waiting)>,
    /// The hub connection, as the terminal sees it.
    pub(crate) link: Link,
    /// The constructed `start` lines with no answer yet, by command id.
    starts: HashMap<String, String>,
    /// Commands waiting for their answer: kind and the draft they carried.
    pub(crate) pending: HashMap<String, (Kind, Draft)>,
    /// The id and line of every fold-cruising hub line with no answer
    /// yet, in the order their waits registered.
    hub_sent: Vec<(CommandId, String)>,
    /// Lines held until the hub connects: the `start` of an early Enter,
    /// with the id each goes out under once the hub speaks `hub_hello`.
    pub(crate) held: Vec<Sent>,
    /// Session commands written and not yet answered, in the order they
    /// went out; no id appears twice.
    pub(crate) kept: Vec<Kept>,
}

impl Outbox {
    /// Registers the feature owning an id, returning any replaced owner.
    /// A repeated id keeps its place in send order.
    pub(crate) fn wait(&mut self, id: &str, waiting: Waiting) -> Option<Waiting> {
        if let Some((_, owner)) = self.waits.iter_mut().find(|(wait, _)| wait == id) {
            return Some(std::mem::replace(owner, waiting));
        }
        self.waits.push((id.to_owned(), waiting));
        None
    }

    /// Peeks at the owner before any observer settles the answer.
    pub(crate) fn waiting(&self, id: &str) -> Option<Waiting> {
        self.tags()
            .find(|(wait, _)| *wait == id)
            .map(|(_, owner)| owner.clone())
    }

    /// Takes a feature's wait once, without settling its draft.
    pub(crate) fn settled(&mut self, id: &str) -> Option<Waiting> {
        let at = self.waits.iter().position(|(wait, _)| wait == id)?;
        self.purge_hub(id);
        Some(self.waits.remove(at).1)
    }

    /// Every registered tag with its command id, in send order.
    pub(crate) fn tags(&self) -> impl Iterator<Item = (&str, &Waiting)> {
        self.waits.iter().map(|(id, owner)| (id.as_str(), owner))
    }

    /// Removes every tag `matches` accepts.
    pub(crate) fn settle_where(&mut self, matches: impl Fn(&Waiting) -> bool) {
        let mut dropped = Vec::new();
        self.waits.retain(|(id, owner)| {
            if matches(owner) {
                dropped.push(id.clone());
                false
            } else {
                true
            }
        });
        for id in dropped {
            self.purge_hub(&id);
        }
    }

    /// Whether any id waits for this feature, regardless of its payload.
    pub(crate) fn waiting_any(&self, waiting: &Waiting) -> bool {
        self.waits
            .iter()
            .any(|(_, owner)| std::mem::discriminant(owner) == std::mem::discriminant(waiting))
    }

    /// Forgets a feature at the same reset that drops its request details.
    pub(crate) fn forget(&mut self, waiting: &Waiting) {
        self.settle_where(|owner| std::mem::discriminant(owner) == std::mem::discriminant(waiting));
    }

    /// Whether the hub spoke a `hub_hello` this terminal reads, and the
    /// connection has not ended since.
    pub(crate) fn is_up(&self) -> bool {
        self.link == Link::Up
    }

    /// Records a made command.
    pub(crate) fn insert(&mut self, id: String, kind: Kind, draft: Draft) {
        self.pending.insert(id, (kind, draft));
    }

    /// The retained `start` line for `id`, if a start sent it and no
    /// answer settled it yet.
    pub(crate) fn start_line(&self, id: &str) -> Option<String> {
        self.starts.get(id).cloned()
    }

    /// Records a made command, sending its line when the link is up and
    /// holding it until `hub_hello` otherwise.
    pub(crate) fn send(
        &mut self,
        id: String,
        kind: Kind,
        draft: Draft,
        line: String,
    ) -> Option<String> {
        if kind == Kind::Start {
            self.starts.insert(id.clone(), line.clone());
        }
        self.pending.insert(id.clone(), (kind, draft));
        if self.is_up() {
            return Some(line);
        }
        // An Enter before the hub connects is held, not lost: `start`
        // goes out once the hub speaks `hub_hello`, paired in the
        // registry then with the line as flushed.
        let sent = Sent {
            id: CommandId(id),
            line,
        };
        self.held.push(sent);
        None
    }

    /// Pairs a fold-cruising hub line with its carried id, beside the
    /// wait the line's answer settles.
    pub(crate) fn note_hub_sent(&mut self, id: &CommandId, line: &str) {
        self.hub_sent.push((id.clone(), line.to_owned()));
    }

    /// Pairs a fold-cruising hub line with its carried id and returns
    /// the line: one step for the fold tails that note each id and
    /// send each line in order.
    pub(crate) fn fold_line(&mut self, sent: Sent) -> String {
        self.note_hub_sent(&sent.id, &sent.line);
        sent.line
    }

    /// The carried id of a fold-cruising hub line, if noted when it was
    /// built: a failed write settles by it instead of parsing the line.
    pub(crate) fn hub_id(&self, line: &str) -> Option<CommandId> {
        self.hub_sent
            .iter()
            .find(|(_, noted)| noted == line)
            .map(|(id, _)| id.clone())
    }

    /// Drops a command's hub-line pairs: a missed purge degrades to the
    /// parse fallback for that line, never to a wrong id.
    fn purge_hub(&mut self, id: &str) {
        self.hub_sent.retain(|(sent, _)| sent.0 != id);
    }

    /// Pops a pending command, returning the kind and draft applying its
    /// failure needs, without reaching the queue, the offer, the phase
    /// or the draft. Its hub-line pair goes with the entry, so a later
    /// disconnect cannot settle by it again.
    pub(crate) fn fail(&mut self, id: &str) -> Option<(Kind, Draft)> {
        self.purge_hub(id);
        self.starts.remove(id);
        self.pending.remove(id)
    }

    /// A command's answer: its pending entry closes, and its line is no
    /// longer kept for resending. Its hub-line pair goes with the
    /// entry, so a repeated disconnect retains no completed line.
    pub(crate) fn answered(&mut self, id: &str) -> Option<(Kind, Draft)> {
        self.purge_hub(id);
        self.kept.retain(|kept| kept.id != id);
        self.starts.remove(id);
        self.pending.remove(id)
    }

    /// The kept commands for a reconnect, in the order they went out:
    /// each command's session with its line as written, so the caller
    /// interleaves the subscribes they need.
    pub(crate) fn reconnect_lines(&self) -> Vec<(SessionId, String)> {
        self.kept
            .iter()
            .map(|kept| (kept.session.clone(), kept.line.clone()))
            .collect()
    }

    /// A line was written to the hub: a session command other than
    /// `subscribe` and `commands`, which a reconnect sends afresh, is kept
    /// until its answer arrives.
    pub(crate) fn wrote(&mut self, line: &str) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let (Some(id), Some(session)) = (
            value.get("id").and_then(Value::as_str),
            value.get("session_id").and_then(Value::as_str),
        ) else {
            return;
        };
        if matches!(
            value.get("command").and_then(Value::as_str),
            Some("subscribe" | "commands")
        ) || self.kept.iter().any(|kept| kept.id == id)
        {
            return;
        }
        self.kept.push(Kept {
            id: id.to_owned(),
            session: SessionId(session.to_owned()),
            line: line.to_owned(),
        });
    }

    /// Whether `id` is a written command kept for resending: a failed
    /// resend leaves its pending entry alone, so the next connection sends
    /// it again (`docs/invocation.md`, "The command line").
    pub(crate) fn is_kept(&self, id: &str) -> bool {
        self.kept.iter().any(|kept| kept.id == id)
    }
}

/// A command line for the hub: `command` serialized as a
/// [`HubCommandLine`], with no `session_id` key, written as one JSON
/// object line (`docs/invocation.md`, "The command line"). Key order is
/// not a wire rule: the hub and the readers parse by key. The fallback
/// never fires for typed structs.
pub(crate) fn hub_line(id: &CommandId, command: HubCommand) -> String {
    serde_json::to_value(HubCommandLine {
        id: id.clone(),
        command,
    })
    .unwrap_or(Value::Null)
    .to_string()
}

/// A command line for a session: `command` serialized as a
/// [`CommandLine`] with `session_id` beside `id`, `command` and `args`,
/// written as one JSON object line (`docs/invocation.md`, "The command
/// line"). Key order is not a wire rule: the hub and the readers parse by
/// key. The fallback never fires for typed structs.
pub(crate) fn session_line(id: &CommandId, session: &SessionId, command: Command) -> String {
    let mut line = serde_json::to_value(CommandLine {
        id: id.clone(),
        command,
    })
    .unwrap_or(Value::Null);
    if let Some(object) = line.as_object_mut() {
        object.insert("session_id".to_owned(), Value::String(session.0.clone()));
    }
    line.to_string()
}

/// The `history` args for `segment`'s window, read under the serving
/// session `own`: `session_id` names the segment only for an ancestor
/// range, absent meaning the serving session itself; `from_seq` and
/// `to_seq` bound the window, each absent meaning its open end.
pub(crate) fn history_args(
    own: &SessionId,
    segment: &SessionId,
    from: Option<Seq>,
    to: Option<Seq>,
) -> HistoryArgs {
    HistoryArgs {
        session_id: (segment != own).then(|| segment.clone()),
        from_seq: from,
        to_seq: to,
    }
}

#[cfg(test)]
#[path = "outbox_tests.rs"]
mod tests;
