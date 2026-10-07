//! Payloads of `docs/events.md`, "Actions", "Approval" and "Interactions".

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::shapes::{Choice, ContentPart, DeclaredEffects, Failure, Process, Question, True};
use crate::{ActionId, CommandId, ProviderCallId, RequestId};

/// Text added since the last delta, on `assistant_message_delta` and
/// `reasoning_delta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextDelta {
    /// The text added.
    pub text: String,
}

/// How a model call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageOutcome {
    /// It completed.
    Completed,
    /// It failed.
    Failed,
}

/// `text_completed`: one text part of a reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextCompleted {
    /// The part's text; `""` for a part the provider sent with only opaque
    /// data.
    pub text: String,
    /// The provider's own form of the part, with its thought signature or
    /// other opaque data. Sent back unchanged only to the model that produced
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_item: Option<Value>,
}

/// `assistant_message_completed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessageCompleted {
    /// How the call ended.
    pub outcome: MessageOutcome,
    /// On `failed` (`docs/errors.md`, "A failed model call").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Failure>,
}

/// `tool_call_arguments_delta`: a tool call the model is still emitting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallArgumentsDelta {
    /// The call's position within the message, from 0.
    pub index: u32,
    /// The tool's name, once the provider has sent it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The raw argument text added since the last delta.
    pub text: String,
}

/// `reasoning_completed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasoningCompleted {
    /// The readable reasoning; `""` when the provider sent none.
    pub text: String,
    /// The provider's reasoning item exactly as it arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_item: Option<Value>,
}

/// `tool_call_requested`: the model finished emitting the call, or an
/// extension ran an inner call. A line with `repaired` or `repairs` but not
/// both fails to read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ToolCallRequestedLine")]
pub struct ToolCallRequested {
    /// The tool's name as the model called it.
    pub name: String,
    /// The arguments as the model sent them: an object, or a string holding
    /// the raw text when it was not JSON.
    pub arguments: Value,
    /// The provider's own id for the call; absent when the reply carried none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<ProviderCallId>,
    /// The repair, absent when nothing was repaired.
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub repair: Option<ArgumentRepair>,
    /// On an inner call, its extension and anchor; absent on the model's calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_by: Option<RanBy>,
    /// On a call the provider ran itself, such as a hosted web search, the
    /// provider's call block exactly as it arrived, sent back unchanged only
    /// to the model that produced it (`docs/tools.md`, "Hosted by the
    /// provider").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_item: Option<Value>,
}

/// An inner call's extension and the outer call or command it ran inside
/// (`docs/events.md`, "`ran_by`"). A line with both anchors or neither fails
/// to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RanByLine", into = "RanByLine")]
pub struct RanBy {
    /// The extension's name.
    pub extension: String,
    /// What the inner call ran inside.
    pub anchor: Anchor,
}

/// What an inner call ran inside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    /// The extension's own tool call.
    OuterCall(ActionId),
    /// The invocation of one of the extension's commands.
    Command(CommandId),
}

/// `ran_by` as a line carries it, before its anchor is checked.
#[derive(Serialize, Deserialize)]
struct RanByLine {
    extension: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outer_action_id: Option<ActionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command_id: Option<CommandId>,
}

impl TryFrom<RanByLine> for RanBy {
    type Error = &'static str;

    fn try_from(line: RanByLine) -> Result<Self, Self::Error> {
        let anchor = match (line.outer_action_id, line.command_id) {
            (Some(id), None) => Anchor::OuterCall(id),
            (None, Some(id)) => Anchor::Command(id),
            _ => return Err("`ran_by` carries exactly one of `outer_action_id` and `command_id`"),
        };
        Ok(Self {
            extension: line.extension,
            anchor,
        })
    }
}

impl From<RanBy> for RanByLine {
    fn from(ran_by: RanBy) -> Self {
        let (outer_action_id, command_id) = match ran_by.anchor {
            Anchor::OuterCall(id) => (Some(id), None),
            Anchor::Command(id) => (None, Some(id)),
        };
        Self {
            extension: ran_by.extension,
            outer_action_id,
            command_id,
        }
    }
}

/// The arguments after repair and the fixes that made them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArgumentRepair {
    /// The arguments after repair (`docs/tools.md`, "Before a call runs").
    pub repaired: Map<String, Value>,
    /// One entry per fix.
    pub repairs: Vec<Repair>,
}

/// `tool_call_requested` as a line carries it, before `repaired` and
/// `repairs` are checked to come together.
#[derive(Deserialize)]
struct ToolCallRequestedLine {
    name: String,
    arguments: Value,
    provider_id: Option<ProviderCallId>,
    repaired: Option<Map<String, Value>>,
    repairs: Option<Vec<Repair>>,
    ran_by: Option<RanBy>,
    provider_item: Option<Value>,
}

impl TryFrom<ToolCallRequestedLine> for ToolCallRequested {
    type Error = &'static str;

    fn try_from(line: ToolCallRequestedLine) -> Result<Self, Self::Error> {
        let repair = match (line.repaired, line.repairs) {
            (Some(repaired), Some(repairs)) => Some(ArgumentRepair { repaired, repairs }),
            (None, None) => None,
            _ => return Err("`repaired` and `repairs` come together or not at all"),
        };
        Ok(Self {
            name: line.name,
            arguments: line.arguments,
            provider_id: line.provider_id,
            repair,
            ran_by: line.ran_by,
            provider_item: line.provider_item,
        })
    }
}

/// One fix made to a tool call's arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repair {
    /// A JSON Pointer into `arguments`.
    pub path: String,
    /// What was done there.
    pub fix: RepairFix,
}

/// How an argument was repaired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairFix {
    /// A `null` sent for an optional property was dropped.
    NullDropped,
    /// A string became the number the schema wants.
    StringToNumber,
    /// A string became the boolean the schema wants.
    StringToBoolean,
    /// A string holding JSON became the value it holds.
    StringParsed,
}

/// `tool_call_started`: execution began.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallStarted {
    /// What the call declared.
    #[serde(flatten)]
    pub declared: DeclaredEffects,
    /// The arguments that ran, when a `before_tool` hook rewrote them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Map<String, Value>>,
    /// With `arguments`, the extensions whose hooks rewrote them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<Vec<String>>,
}

/// Streamed output and progress, on `tool_call_delta` and `job_delta`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    /// Output added since the last delta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Progress for clients, shaped as the tool chooses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// How a tool call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    /// It completed.
    Completed,
    /// It failed.
    Failed,
    /// It was denied.
    Denied,
    /// Fiber or the user stopped it.
    Cancelled,
}

/// `tool_call_completed`: the call's outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallCompleted {
    /// How it ended.
    pub status: CallStatus,
    /// On `denied`, why; an open set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// On `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Failure>,
    /// On any call that ran a process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<Process>,
    /// Exactly what the model is sent.
    pub content: Vec<ContentPart>,
    /// Data for clients, such as an edit's diff; never sent to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// The full output's path, when the result was cut or a hook returned text
    /// for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// On a call that changed files, one entry per file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<Vec<FileChange>>,
    /// Instructions to the loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Control>,
    /// When an `after_tool` hook rewrote the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_by: Option<Vec<String>>,
    /// On a call the provider ran, its result block exactly as it arrived,
    /// sent back unchanged only to the model that produced it
    /// (`docs/tools.md`, "Hosted by the provider").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_item: Option<Value>,
}

/// One file a call changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// The file.
    pub path: String,
    /// Lines added.
    pub added: u64,
    /// Lines removed.
    pub removed: u64,
}

/// A tool result's instructions to the loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Control {
    /// A handoff note (`docs/handoff.md`).
    pub handoff: String,
}

/// Which step of `docs/permissions.md`, "The order a call is judged in",
/// raised a permission request, keyed by `step`, with the keys that step
/// defines.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum AskStep {
    /// Step 3, a standing rule that asks.
    StandingAsk {
        /// The rule that asked.
        standing_rule: StandingRule,
    },
    /// Step 7, review.
    Review {
        /// In `auto`, why the reviewer handed the call to a person.
        #[serde(skip_serializing_if = "Option::is_none")]
        escalation: Option<Escalation>,
        /// The rule an allow can remember; absent when no rule can match the
        /// call.
        #[serde(skip_serializing_if = "Option::is_none")]
        rule: Option<RuleOffer>,
    },
}

/// `step` alone, as a line carries it.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum StepName {
    StandingAsk,
    Review,
}

/// Where a standing rule lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleScope {
    /// The global rules file.
    Global,
    /// The project's rules file.
    Project,
}

/// The standing rule that asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandingRule {
    /// Where the rule lives.
    pub scope: RuleScope,
    /// The rule's prefix.
    pub prefix: String,
}

/// Why the reviewer handed a call to a person, keyed by `cause`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cause", rename_all = "snake_case")]
pub enum Escalation {
    /// Too many blocks in a row.
    ConsecutiveBlocks {
        /// The reviewer's reason for blocking this call.
        reason: String,
    },
    /// Too many blocks in the session.
    SessionBlocks {
        /// The reviewer's reason for blocking this call.
        reason: String,
    },
    /// The reviewer failed.
    ReviewerFailed {
        /// Why.
        error: Failure,
    },
}

/// The rule an allow can remember (`docs/permissions.md`, "What a rule
/// matches").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleOffer {
    /// The call's primary argument as its tool reads it.
    pub subject: String,
    /// The widening the tool offers, which `subject` starts with.
    pub prefix: String,
}

/// `permission_requested`. The envelope's `action_id` is the tool call. A
/// line with a key its step does not define fails to read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PermissionRequestedLine")]
pub struct PermissionRequested {
    /// The id a `reply` names.
    pub request_id: RequestId,
    /// What the call declared.
    #[serde(flatten)]
    pub declared: DeclaredEffects,
    /// Which step raised it, with its keys.
    #[serde(flatten)]
    pub step: AskStep,
}

/// `permission_requested` as a line carries it, before its keys are checked
/// against its step.
#[derive(Deserialize)]
struct PermissionRequestedLine {
    request_id: RequestId,
    #[serde(flatten)]
    declared: DeclaredEffects,
    step: StepName,
    standing_rule: Option<StandingRule>,
    escalation: Option<Escalation>,
    rule: Option<RuleOffer>,
}

impl TryFrom<PermissionRequestedLine> for PermissionRequested {
    type Error = &'static str;

    fn try_from(line: PermissionRequestedLine) -> Result<Self, Self::Error> {
        let step = match (line.step, line.standing_rule, line.escalation, line.rule) {
            (StepName::StandingAsk, Some(standing_rule), None, None) => {
                AskStep::StandingAsk { standing_rule }
            }
            (StepName::Review, None, escalation, rule) => AskStep::Review { escalation, rule },
            _ => return Err("a permission request carries only the keys its step defines"),
        };
        Ok(Self {
            request_id: line.request_id,
            declared: line.declared,
            step,
        })
    }
}

/// A permission decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Allow the call.
    Allow,
    /// Deny the call.
    Deny,
}

/// Who or what decided a permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecidedBy {
    /// The credential deny.
    CredentialDeny,
    /// A person.
    Person,
    /// A standing rule.
    StandingRule,
    /// A session grant.
    SessionGrant,
    /// The reviewer.
    Reviewer,
    /// The turn was cancelled while the request was pending.
    Cancel,
}

/// A tool and prefix a person's allow added, as a session grant or a standing
/// rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// The tool.
    pub tool: String,
    /// The prefix later calls match.
    pub prefix: String,
}

/// The reviewer that decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerRef {
    /// A model reference.
    pub model: String,
    /// `1` or `2`.
    pub stage: u8,
}

/// `permission_resolved`. The envelope's `action_id` is the tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionResolved {
    /// The request it answers; absent when the decision raised none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    /// The decision.
    pub decision: Decision,
    /// Who or what decided.
    pub decided_by: DecidedBy,
    /// Why, in words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What the person typed with a denial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    /// On a person's allow that added a session grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<Grant>,
    /// On a person's allow that added a standing rule to the project's rules
    /// file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<Grant>,
    /// When `decided_by` is `reviewer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<ReviewerRef>,
}

/// What an interaction asks, keyed by `kind`, with the keys that kind
/// defines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Interaction {
    /// Yes or no.
    Confirm {
        /// The question.
        prompt: String,
    },
    /// One option.
    Select {
        /// The question.
        prompt: String,
        /// The options.
        options: Vec<Choice>,
    },
    /// Any number of options.
    MultiSelect {
        /// The question.
        prompt: String,
        /// The options.
        options: Vec<Choice>,
    },
    /// Typed text.
    TextInput {
        /// The question.
        prompt: String,
    },
    /// Several questions.
    Form {
        /// One per question.
        fields: Vec<Question>,
    },
}

/// `kind` alone, as a line carries it.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum InteractionKind {
    Confirm,
    Select,
    MultiSelect,
    TextInput,
    Form,
}

/// `interaction_requested`. The envelope carries no `action_id`. A line with
/// a key its kind does not define fails to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "InteractionRequestedLine")]
pub struct InteractionRequested {
    /// The id a `reply` names.
    pub request_id: RequestId,
    /// What it asks.
    #[serde(flatten)]
    pub interaction: Interaction,
    /// The tool calls that raised it; absent when no tool call raised it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_ids: Option<Vec<ActionId>>,
    /// The extension that raised it with `host.ask`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
}

/// `interaction_requested` as a line carries it, before its keys are checked
/// against its kind.
#[derive(Deserialize)]
struct InteractionRequestedLine {
    request_id: RequestId,
    kind: InteractionKind,
    action_ids: Option<Vec<ActionId>>,
    extension: Option<String>,
    prompt: Option<String>,
    options: Option<Vec<Choice>>,
    fields: Option<Vec<Question>>,
}

impl TryFrom<InteractionRequestedLine> for InteractionRequested {
    type Error = &'static str;

    fn try_from(line: InteractionRequestedLine) -> Result<Self, Self::Error> {
        use InteractionKind as K;
        let interaction = match (line.kind, line.prompt, line.options, line.fields) {
            (K::Confirm, Some(prompt), None, None) => Interaction::Confirm { prompt },
            (K::Select, Some(prompt), Some(options), None) => {
                Interaction::Select { prompt, options }
            }
            (K::MultiSelect, Some(prompt), Some(options), None) => {
                Interaction::MultiSelect { prompt, options }
            }
            (K::TextInput, Some(prompt), None, None) => Interaction::TextInput { prompt },
            (K::Form, None, None, Some(fields)) => Interaction::Form { fields },
            _ => return Err("an interaction request carries only the keys its kind defines"),
        };
        Ok(Self {
            request_id: line.request_id,
            interaction,
            action_ids: line.action_ids,
            extension: line.extension,
        })
    }
}

/// Who resolved an interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedBy {
    /// A client's `reply`.
    Person,
    /// Fiber, which declines when no answer is possible or the turn was
    /// cancelled.
    Fiber,
}

/// `interaction_resolved`. It carries exactly one answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionResolved {
    /// The request it answers.
    pub request_id: RequestId,
    /// Who resolved it.
    pub by: ResolvedBy,
    /// The answer.
    #[serde(flatten)]
    pub answer: Answer,
}

/// An interaction's answer: `declined`, or the answer keys for its kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Answer {
    /// Declined.
    Declined {
        /// `true`.
        declined: True,
    },
    /// The answer to `confirm`.
    Confirmed {
        /// Yes or no.
        confirmed: bool,
    },
    /// The chosen options' labels: one on `select`, possibly none on
    /// `multi_select`.
    Labels {
        /// The labels.
        labels: Vec<String>,
    },
    /// The typed answer, on `text_input`.
    Text {
        /// The text.
        text: String,
    },
    /// The answers to a `form`.
    Form {
        /// One per field, in field order.
        answers: Vec<FormAnswer>,
        /// The person's note on the whole form.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
}

/// One field's answer on a `form`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FormAnswer {
    /// The person skipped the field.
    Skipped {
        /// `true`.
        skipped: True,
    },
    /// The person answered it.
    Answered {
        /// The chosen labels, possibly none.
        labels: Vec<String>,
        /// What the person typed, when they typed any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}
