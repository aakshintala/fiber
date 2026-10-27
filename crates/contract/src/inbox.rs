//! What a driver, an extension or another session sends to a session's
//! loop (`docs/architecture.md`, "One inbox"). The loop owns the receiving
//! end; everyone else holds a sender, so this type lives here and no sender
//! depends on `loop`.

use std::fmt;

use crate::shapes::{ContentPart, Sender};

/// A message for the loop, carried by [`Delivery::Prompt`] or
/// [`Delivery::Steer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The message.
    pub content: Vec<ContentPart>,
    /// Where it came from.
    pub sender: Sender,
}

/// What an acknowledgement carries. `Ok(None)` accepts a command that
/// answers nothing; `Ok(Some)` accepts one that answers with a result;
/// `Err` rejects it. The loop never writes an answer to the log
/// (`docs/invocation.md`, "Driver commands").
pub type Answer = Result<Option<crate::events::CommandResult>, Rejection>;

/// Why a command was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The rejection code (`docs/invocation.md`, "Driver commands").
    pub code: crate::ErrorCode,
    /// Fiber's own sentence.
    pub message: String,
}

/// Called once with the command's answer. The loop calls it when it takes
/// the delivery, except a prompt that starts a turn, which it calls once
/// that turn's `turn_started` is written. Dropping an [`Ack`] uncalled is
/// the sender's concern: the loop stops on a log error without calling the
/// ones it has not taken.
pub struct Ack(pub Box<dyn FnOnce(Answer) + Send>);

impl fmt::Debug for Ack {
    // The `"Ack(..)"` string is a developer aid no doc promises and no consumer observes.
    #[cfg_attr(false, mutants::skip)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ack(..)")
    }
}

/// A command's `host.tool` request (`docs/extensions.md`, "Running a tool"):
/// the loop runs it as an inner call anchored on the command's invocation.
pub struct InnerCallRequest {
    /// The extension running the command.
    pub extension: String,
    /// The `command` driver command's invocation id.
    pub command: crate::CommandId,
    /// The tool to run.
    pub name: String,
    /// The arguments supplied by the command.
    pub arguments: serde_json::Value,
    /// Set once the command's call ends, for any reason.
    pub cancel: std::sync::Arc<dyn crate::tool::Cancel>,
    /// Called once by the loop, before the first line and first hook.
    pub taken: Box<dyn FnOnce() + Send>,
    /// Called once with the completion the loop wrote. Dropping this
    /// uncalled answers it `cancelled` on the requester's side.
    pub answer: Box<dyn FnOnce(Box<crate::events::ToolCallCompleted>) + Send>,
}

impl fmt::Debug for InnerCallRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InnerCallRequest")
            .field("extension", &self.extension)
            .field("command", &self.command)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// A session message (`docs/tools.md`, "Messaging other sessions"),
/// answered by the door once it is in the inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMessage {
    /// The sending session's id.
    pub from_session_id: crate::SessionId,
    /// The `message` command that sent it.
    pub command_id: crate::CommandId,
    /// The message text, as sent.
    pub text: String,
    /// The latest `session_named` name in the sender's log; `None` when it
    /// has none, cleared it, or its log could not be read.
    pub name: Option<String>,
    /// The sender's parent session, when the sender has one.
    pub parent_session_id: Option<crate::SessionId>,
}

/// What the loop's inbox carries (`docs/invocation.md`, "Driver commands").
#[allow(
    clippy::large_enum_variant,
    reason = "a delegate's finish rides its job's notice; boxing would allocate on every job end"
)]
#[derive(Debug)]
pub enum Delivery {
    /// Starts a turn. Rejected `busy` while one is running, and `closing`
    /// after `close`.
    Prompt(Message, Ack),
    /// A steering message. While a turn runs it joins that turn at the next
    /// step boundary; while the loop is idle it starts a turn.
    Steer(Message, Ack),
    /// Removes the steering message sent by the `steer` command `CommandId`
    /// names, when that message is still unapplied.
    SteerDrop(crate::CommandId, Ack),
    /// A session message from another session (`docs/tools.md`, "Messaging
    /// other sessions"). The ack answers once the loop takes it: accepted
    /// after the `before_message` hook passed and the message is queued,
    /// or rejected `message_refused`, `hook_failed` or `closing`.
    Message(SessionMessage, Ack),
    /// Switches model or thinking at the next turn boundary.
    Model(crate::commands::ModelArgs, Ack),
    /// Switches the credential label at the next turn boundary.
    Credential(crate::commands::CredentialArgs, Ack),
    /// Re-reads the session's configuration and declares its tools again.
    Reload(Ack),
    /// Restarts one failed MCP server (`docs/mcp.md`, "Servers, their
    /// state and the per-server commands"). Rejected `busy` while a turn
    /// runs and `closing` after `close`, as a reload is; the loop
    /// answers it once the restart's lines are written.
    McpReconnect(crate::commands::McpServerArgs, Ack),
    /// The person's `name` command: written as `session_named` by
    /// `person` at the drain that takes it, then acknowledged `Ok(None)`.
    /// Refused `closing` while a shutdown settles and after `rewound`; a
    /// drain that takes it before settling, such as an approval wait
    /// reading a set shutdown signal, writes and accepts it as at any
    /// drain.
    Name(crate::commands::Name, Ack),
    /// Starts a new session that continues this one from an earlier point
    /// (`docs/events.md`, "Rewind").
    Rewind(crate::CommandId, crate::commands::RewindArgs, Ack),
    /// Starts a new session that continues this one in another workspace
    /// (`docs/events.md`, "Change workspace").
    ChangeWorkspace(crate::CommandId, crate::commands::ChangeWorkspaceArgs, Ack),
    /// A person's `handoff`. While a turn runs it is held until the next
    /// step boundary; while the loop is idle it starts a turn of its own
    /// (`docs/handoff.md`, "A person").
    Handoff(crate::CommandId, crate::commands::Handoff, Ack),
    /// A person's answer to a pending approval.
    Reply(crate::commands::Reply, Ack),
    /// Accept no more prompts. The turn in flight finishes, then the loop
    /// exits.
    Close(Ack),
    /// A background job ended (`docs/tools.md`, "Background jobs"). The
    /// loop takes it as news for the model only when its claim holds.
    Job(JobNotice),
    /// A batch of a monitor's lines (`docs/tools.md`, "Background jobs").
    /// It wakes the model as a job's end does; no claim applies.
    JobLine(crate::events::JobLine),
    /// A program an extension ran outside a tool call: written as
    /// `extension_exec`, starts nothing.
    ExtensionExec(crate::events::ExtensionExec),
    /// An extension's delegate started: written as `job_started` then
    /// `delegate_started`, without a turn or action. Sent before its end.
    ExtensionJob(crate::events::JobStarted, crate::events::DelegateStarted),
    /// An extension's `host.ask` question: written at the drain that takes
    /// it as `interaction_requested`, with no turn and no action.
    Interaction(crate::events::InteractionRequested),
    /// An extension ask's answer: written as `interaction_resolved`, then
    /// the ack is called with `Ok(None)`.
    Resolved(crate::events::InteractionResolved, Ack),
    /// A command's inner call, run alone while idle or at a step boundary.
    InnerCall(InnerCallRequest),
    /// A line an extension wrote with `host.log`: shown live, recorded in
    /// the diagnostic log, starts nothing.
    ExtensionLog(crate::events::ExtensionLog),
    /// One `usage_recorded` from an extension's `host.model` call: written
    /// and ledgered with no turn and no action.
    ExtensionUsage(crate::events::UsageRecorded),
    /// One `usage_recorded` a Fiber delegate's socket delivered: copied
    /// into the parent's log with the originator named, folded once per
    /// `generation_id` (`docs/delegates.md`, "Streams").
    DelegateUsage {
        /// The delegate session that sent the line.
        session: crate::SessionId,
        /// The line as the delegate wrote it.
        line: crate::events::UsageRecorded,
    },
    /// Wakes a loop blocked on its inbox after an accepted `cancel` or a
    /// clock move. It carries no ack and no meaning: every drain discards
    /// it, and an approval wait reads the cancel signal after it wakes
    /// (`docs/architecture.md`, "Cancellation").
    Cancelled,
    /// A driver `shell` with `send` true, run as `send` false is: the loop
    /// writes it as `shell_command` at the drain that takes it, then
    /// answers `Ok(None)` (`docs/invocation.md`, "Shell").
    Shell(crate::events::ShellCommand, Ack),
    /// Proposes reviewer-note lines between turns, without conversation.
    ProposeNotes(crate::notes::NotesAsk, Ack),
}

/// News that a background job ended, sent by the session's jobs registry.
/// The loop asks the claim at admission while idle, or at the step
/// boundary that writes the notice while a turn runs.
#[derive(Debug)]
pub struct JobNotice {
    /// How the job ended: the `job_completed` the loop writes with the
    /// notice.
    pub completed: crate::events::JobCompleted,
    /// Whether this notice is still the model's news to receive: `None`
    /// once a `wait` or `stop` already returned the job's final state, and
    /// `Some` of the queued records taken ahead of the notice's own two
    /// otherwise.
    pub claim: Claim,
    /// A Fiber delegate's finish, written as `delegate_finished` before
    /// `job_completed` when the loop writes the notice. The text is
    /// unbounded; the loop cuts it once, at the write.
    pub delegate: Option<crate::events::DelegateFinished>,
}

/// True for exactly one caller across this notice and the job's `wait` and
/// `stop`: `None` once a `jobs wait` or `stop` already returned the job's
/// final state to the model, so the refused claim takes nothing.
/// `Some(ahead)` means the claim holds, and `ahead` lists the records the
/// claim took from the job's queue ahead of the notice's own
/// `delegate_finished` and `job_completed`, in queue order, for the loop
/// to write first.
pub struct Claim(pub Box<dyn FnOnce() -> Option<Vec<crate::jobs::Taken>> + Send>);

impl fmt::Debug for Claim {
    // The `"Claim(..)"` string is a developer aid no doc promises and no consumer observes.
    #[cfg_attr(false, mutants::skip)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Claim(..)")
    }
}
