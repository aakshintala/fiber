//! Payloads of `docs/events.md`, "Process boundary" and "Session and turn".

use serde::{Deserialize, Serialize};

use crate::shapes::{ContentPart, Failure, Mode, Point, Process, Question, Sender};
use crate::{ActionId, CommandId, JobId, RequestId, Seq, SessionId};

/// `fiber_started`: the first line a process writes for a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiberStarted {
    /// The Fiber version, such as `0.0.1`.
    pub version: String,
    /// `false` for a new session, `true` for a resumed one.
    pub resumed: bool,
    /// The permission mode this process opened the session in.
    pub mode: Mode,
}

/// `fiber_exited`: the last line a process writes for a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FiberExited {
    /// The process's exit code (`docs/invocation.md`, "Lifecycle").
    pub exit_code: i32,
    /// The final assistant message, when there is one.
    #[serde(flatten)]
    pub final_message: Option<FinalMessage>,
    /// Why the process failed (`docs/errors.md`, "What a caller gets").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Failure>,
    /// The pending approval or question the process exited on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended_on: Option<RequestId>,
    /// Copied from the last `turn_completed`, when its turn ended on questions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<Question>>,
}

/// The final message `fiber_exited` points at and copies. The copy is output,
/// never a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalMessage {
    /// The final assistant message's action.
    pub final_action_id: ActionId,
    /// That message's text.
    pub text: String,
}

/// `session_started`: the first line of every session's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStarted {
    /// The workspace root.
    pub workspace: String,
    /// For a delegate, its parent (`docs/delegates.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Parent>,
    /// For a fork or a rewind, the point it continues from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<Point>,
    /// For a rewind, what rides after the history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewind: Option<Rewind>,
}

/// A delegate's parent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parent {
    /// The parent session.
    pub session_id: SessionId,
    /// The delegate's `job_id` there.
    pub delegate_id: JobId,
}

/// What a rewound session's model receives after the history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rewind {
    /// The summary of the path after the point, when one was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Fiber's note: files written, shell calls and jobs since the point.
    pub note: String,
    /// The jobs adopted.
    pub jobs: Vec<JobId>,
}

/// `rewound`: the last line of a session that was rewound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rewound {
    /// The session that continues this one.
    pub new_session_id: SessionId,
    /// The point.
    pub seq: Seq,
    /// The jobs handed to the new session; empty when none.
    pub jobs: Vec<JobId>,
}

/// `turn_started`. Its envelope carries the new `turn_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStarted {
    /// Everything that started the turn, in arrival order.
    pub input: Vec<InputItem>,
}

/// One thing that started a turn, keyed by `type`. The set is open: an item
/// this build does not know reads as [`InputItem::Unknown`], which a consumer
/// skips and never writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputItem {
    /// A message from a driver, an extension or another session.
    Message {
        /// The message.
        content: Vec<ContentPart>,
        /// Where it came from.
        #[serde(flatten)]
        sender: Sender,
        /// The extensions whose hooks changed it, in the order they ran.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        changed_by: Option<Vec<String>>,
    },
    /// A `shell_command` line since the last turn.
    ShellCommand {
        /// That line's `seq`.
        seq: Seq,
    },
    /// Jobs whose news started the turn.
    Jobs {
        /// The jobs.
        job_ids: Vec<JobId>,
    },
    /// A `handoff` command sent between turns.
    Handoff {
        /// The command.
        command_id: CommandId,
    },
    /// An item this build does not know.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    /// It completed.
    Completed,
    /// It was cancelled.
    Interrupted,
    /// It failed.
    Failed,
}

/// `turn_completed`: the turn is settled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnCompleted {
    /// How it ended.
    pub outcome: TurnOutcome,
    /// On `failed` (`docs/errors.md`, "What ends a turn").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Failure>,
    /// When an `ask_user` call ended the turn for a driver that is a program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<Question>>,
}

/// `steering_applied`: a steering message a running turn received at a step
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringApplied {
    /// The message as the turn received it.
    pub content: Vec<ContentPart>,
    /// Where it came from; the command is the `steer` or `message` that sent
    /// it.
    #[serde(flatten)]
    pub sender: Sender,
    /// When a hook rewrote the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<Vec<String>>,
}

/// `steering_queue`: every steering message still queued. The latest wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringQueue {
    /// The queued messages, oldest first; empty when the queue is.
    pub messages: Vec<QueuedMessage>,
}

/// One queued steering message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedMessage {
    /// The message.
    pub content: Vec<ContentPart>,
    /// Where it came from.
    #[serde(flatten)]
    pub sender: Sender,
}

/// `shell_command`: a command the person ran with `send` true.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellCommand {
    /// The command as typed.
    pub command: String,
    /// Its output, cut as a shell call's result is.
    pub output: String,
    /// The full output's path, when cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// How it ended.
    pub process: Process,
}

/// Who named a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamedBy {
    /// The `name` command, which pins the name.
    Person,
    /// The model's `name_session`.
    Model,
}

/// `session_named`: the latest is the session's name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionNamed {
    /// The name; `null` when the person cleared theirs.
    pub name: Option<String>,
    /// Who named it.
    pub by: NamedBy,
}

/// `clients`: written whenever a client attaches or leaves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clients {
    /// The clients attached, the receiving client included.
    pub count: u32,
}

/// `context_added`: text a hook added to the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextAdded {
    /// The text.
    pub text: String,
    /// The hook's extension.
    pub extension: String,
    /// The hook point, such as `turn_start`.
    pub hook: String,
}
