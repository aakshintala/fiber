//! Step 7 of the permission order (`docs/permissions.md`, "The reviewer"):
//! a call no earlier step decided is judged by a separate reviewer model.

use std::sync::Arc;

use contract::events::{
    CacheLifetime, DecidedBy, Decision, Escalation, Event, InputItem, Notice, PermissionResolved,
    ReviewerRef, RuleOffer, ToolCallCompleted, ToolCallRequested, UsageRecorded,
};
use contract::provider::{CallError, Cost, Input, ModelRequest, Provider, Reply};
use contract::shapes::{ContentPart, Failure, Origin};
use contract::tool::{Effects, Tool};
use contract::{ActionId, ErrorCode, RequestId, TurnId};
use serde_json::{Map, Value};

use crate::calls::{Approved, AskAbout, Asked, denied};
use crate::{Error, Loop, Model};

/// What step 7 says about one call: it runs, or how its denial reads.
type Decided = Result<Approved, Box<ToolCallCompleted>>;

/// What judges a reviewed call (`docs/permissions.md`, "How it runs").
pub struct Reviewer {
    /// The provider the reviewer's requests go to.
    pub provider: Arc<dyn Provider>,
    /// The reviewer's model, and how its calls are priced.
    pub model: Model,
}

/// When a reviewer block hands the call to a person
/// (`docs/permissions.md`, "What happens on a block").
pub struct BlockLimits {
    /// Consecutive blocks before a person is asked.
    pub consecutive: u64,
    /// Blocks in a session before a person is asked.
    pub session: u64,
}

impl Default for BlockLimits {
    fn default() -> Self {
        Self {
            consecutive: 3,
            session: 20,
        }
    }
}

/// One item of what the reviewer is shown (`docs/permissions.md`, "What it
/// is shown"). A message carries no call.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Reviewed {
    /// The call this item renders; `None` for a person's message.
    pub action: Option<ActionId>,
    /// The item as the reviewer reads it.
    pub input: Input,
}

/// The reviewer's instructions (`docs/system-prompt.md`, "The texts").
pub(crate) struct Sections {
    /// What every reviewer request carries as its system prompt.
    pub shared: String,
    /// The first stage's instruction.
    pub first: String,
    /// The second stage's instruction.
    pub second: String,
}

/// The reviewer's instructions, split per `docs/system-prompt.md`'s rule.
pub(crate) fn sections() -> Sections {
    sections_of(include_str!("../prompt/reviewer.md"))
}

/// Splits `md` into its sections, each from its `## name` line to the next
/// `## ` line, blank lines at either end removed.
fn sections_of(md: &str) -> Sections {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in md.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            groups.push((name, vec![line]));
        } else if let Some((_, lines)) = groups.last_mut() {
            lines.push(line);
        }
    }
    let section = |name: &str| {
        groups
            .iter()
            .find(|(at, _)| *at == name)
            .map_or_else(String::new, |(_, lines)| joined(lines))
    };
    Sections {
        shared: section("shared"),
        first: section("first-pass"),
        second: section("second-pass"),
    }
}

/// `lines` joined, with blank lines at either end removed.
fn joined(lines: &[&str]) -> String {
    let start = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |at| at + 1);
    lines
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

/// Adds what `event` puts in what the reviewer is shown: each person's
/// message and each tool call, nothing else.
pub(crate) fn render_reviewed(
    reviewed: &mut Vec<Reviewed>,
    event: &Event,
    action: Option<&ActionId>,
) {
    match event {
        Event::TurnStarted(started) => {
            for item in &started.input {
                if let InputItem::Message {
                    content, sender, ..
                } = item
                    && sender.origin == Origin::Driver
                {
                    reviewed.push(person(content));
                }
            }
        }
        Event::SteeringApplied(steering) => {
            if steering.sender.origin == Origin::Driver {
                reviewed.push(person(&steering.content));
            }
        }
        Event::ToolCallRequested(call) => {
            if let Some(action) = action {
                let arguments = match &call.repair {
                    Some(repair) => Value::Object(repair.repaired.clone()),
                    None => call.arguments.clone(),
                };
                let rendered = format!(
                    "{{\"tool\":{},\"arguments\":{}}}",
                    serde_json::to_string(&call.name).unwrap_or_default(),
                    serde_json::to_string(&arguments).unwrap_or_default(),
                );
                reviewed.push(Reviewed {
                    action: Some(action.clone()),
                    input: Input::User {
                        text: format!("Tool call: {rendered}"),
                    },
                });
            }
        }
        // Every other kind adds nothing the reviewer reads. Each is
        // listed, so a new kind does not compile until it is placed.
        // Every other kind adds nothing the reviewer reads. Each is
        // listed, so a new kind does not compile until it is placed.
        Event::FiberStarted(_) | Event::FiberExited(_) | Event::SessionStarted(_) => {}
        Event::Rewound(_) | Event::StepStarted(_) | Event::TurnCompleted(_) => {}
        Event::SteeringQueue(_) | Event::ShellCommand(_) | Event::SessionNamed(_) => {}
        Event::Clients(_) | Event::SessionStatus(_) | Event::ContextAdded(_) => {}
        Event::AssistantMessageStarted(_)
        | Event::AssistantMessageDelta(_)
        | Event::AssistantMessageCompleted(_) => {}
        Event::TextCompleted(_) | Event::ToolCallArgumentsDelta(_) | Event::ReasoningStarted(_) => {
        }
        Event::ReasoningDelta(_) | Event::ReasoningCompleted(_) | Event::ToolCallStarted(_) => {}
        Event::ToolCallDelta(_) | Event::ToolCallCompleted(_) | Event::PermissionRequested(_) => {}
        Event::PermissionResolved(_)
        | Event::InteractionRequested(_)
        | Event::InteractionResolved(_) => {}
        Event::UsageRecorded(_) | Event::QuotaNoticed(_) | Event::RetryScheduled(_) => {}
        Event::Notice(_) | Event::PreambleBuilt(_) | Event::ModelChanged(_) => {}
        Event::OpeningMessage(_) | Event::InstructionFile(_) | Event::DateChanged(_) => {}
        Event::HandoffStarted(_) | Event::HandoffCompleted(_) | Event::ContextNudged(_) => {}
        Event::McpServerFailed(_) | Event::McpServerReady(_) | Event::Reloaded(_) => {}
        Event::ExtensionsLoaded(_)
        | Event::ExtensionStateSet(_)
        | Event::ExtensionStateUnset(_) => {}
        Event::ExtensionUi(_) | Event::ExtensionMessage(_) | Event::ExtensionExec(_) => {}
        Event::JobStarted(_) | Event::DelegateStarted(_) | Event::JobDelta(_) => {}
        Event::JobLine(_) | Event::DelegateFinished(_) | Event::JobCompleted(_) => {}
        Event::JobsPendingNotified(_) | Event::CommandAccepted(_) | Event::CommandRejected(_) => {}
    }
}

/// A person's message as the reviewer reads it.
fn person(content: &[ContentPart]) -> Reviewed {
    Reviewed {
        action: None,
        input: Input::User {
            text: format!("The person: {}", crate::conversation::text(content)),
        },
    }
}

/// What the first stage said.
#[derive(Debug)]
pub(crate) enum First {
    Allow,
    Check,
    Unreadable(String),
}

/// What the second stage said.
#[derive(Debug)]
pub(crate) enum Second {
    Allow { reason: Option<String> },
    Block { reason: String },
    Unreadable(String),
}

/// Reads a first-stage verdict: `check` or `allow`, cleaned. A reply cut
/// off by the output limit still reads.
pub(crate) fn read_first(text: &str) -> First {
    match clean(text).as_str() {
        "allow" => First::Allow,
        "check" => First::Check,
        _ => First::Unreadable(format!(
            "expected one word, `check` or `allow`, but got {text:?}"
        )),
    }
}

/// Reads a second-stage verdict: `allow` or `block` plus a reason. A
/// `block` with an empty reason does not read; an `allow` may have none.
pub(crate) fn read_second(text: &str) -> Second {
    let trimmed = text.trim_start();
    let mut words = trimmed.split_whitespace();
    let Some(first) = words.next() else {
        return Second::Unreadable(
            "expected `allow` or `block` with a reason, but got nothing".to_owned(),
        );
    };
    let rest = trimmed
        .strip_prefix(first)
        .unwrap_or("")
        .trim_start_matches(|c: char| c == ':' || c == '-' || c.is_whitespace());
    let reason = rest.trim_end().to_owned();
    match clean(first).as_str() {
        "allow" => Second::Allow {
            reason: (!reason.is_empty()).then_some(reason),
        },
        "block" if !reason.is_empty() => Second::Block { reason },
        "block" => {
            Second::Unreadable("a `block` needs a reason in one sentence, but got none".to_owned())
        }
        _ => Second::Unreadable(format!(
            "expected `allow` or `block` with a reason, but got {text:?}"
        )),
    }
}

/// One word as a verdict reads it.
fn clean(word: &str) -> String {
    let mut cleaned = word.trim().to_ascii_lowercase();
    loop {
        let stripped = cleaned
            .trim_matches(['`', '"', '\''])
            .trim_end_matches('.')
            .trim();
        if stripped == cleaned {
            return stripped.to_owned();
        }
        cleaned = stripped.to_owned();
    }
}

#[cfg(test)]
#[path = "reviewer_tests.rs"]
mod tests;

/// One call under review: everything step 7 needs after the stages read it.
struct UnderReview<'a> {
    call: &'a ToolCallRequested,
    id: &'a ActionId,
    turn: &'a TurnId,
    tool: &'a Arc<dyn Tool>,
    arguments: &'a Map<String, Value>,
    effects: &'a Effects,
}

/// What sends a reviewer request: the reviewer's provider and model, cloned
/// off the loop so a request borrows nothing while it runs.
struct ReviewEndpoint {
    provider: Arc<dyn Provider>,
    reference: String,
    cost: Option<Cost>,
    subscription: bool,
}

/// What one full stage (with its one re-ask) said.
enum StageReply {
    Text(String),
    Fail(Failure),
    Cancelled,
    Budget,
}

/// Reads a first-stage verdict, or says what was wrong.
fn first_readable(text: &str) -> Result<(), String> {
    match read_first(text) {
        First::Allow | First::Check => Ok(()),
        First::Unreadable(why) => Err(why),
    }
}

/// Reads a second-stage verdict, or says what was wrong.
fn second_readable(text: &str) -> Result<(), String> {
    match read_second(text) {
        Second::Allow { .. } | Second::Block { .. } => Ok(()),
        Second::Unreadable(why) => Err(why),
    }
}

impl Loop {
    /// Judges `call` at step 7 (`docs/permissions.md`, "The reviewer").
    pub(crate) fn review(
        &mut self,
        call: &ToolCallRequested,
        id: &ActionId,
        turn: &TurnId,
        tool: Arc<dyn Tool>,
        arguments: Map<String, Value>,
        effects: &Effects,
    ) -> Result<Decided, Error> {
        let prompt = sections();
        if self.review_over_budget() {
            return self.budget_deny(id, turn);
        }
        let under = UnderReview {
            call,
            id,
            turn,
            tool: &tool,
            arguments: &arguments,
            effects,
        };
        let endpoint = match &self.reviewer {
            Ok(reviewer) => ReviewEndpoint {
                provider: Arc::clone(&reviewer.provider),
                reference: reviewer.model.reference.clone(),
                cost: reviewer.model.cost.clone(),
                subscription: reviewer.model.subscription,
            },
            Err(failure) => {
                let failure = failure.clone();
                self.no_model_notice(turn, &failure)?;
                return self.review_failure(&under, failure, None);
            }
        };
        match self.ask_stage(
            &under,
            &endpoint,
            &prompt.shared,
            &prompt.first,
            Some(1),
            first_readable,
        )? {
            StageReply::Text(text) => match read_first(&text) {
                First::Allow => self.second_allow(&under, &endpoint, 1, None),
                First::Check => match self.ask_stage(
                    &under,
                    &endpoint,
                    &prompt.shared,
                    &prompt.second,
                    None,
                    second_readable,
                )? {
                    StageReply::Text(text) => match read_second(&text) {
                        Second::Allow { reason } => self.second_allow(&under, &endpoint, 2, reason),
                        Second::Block { reason } => self.second_block(&under, &endpoint, reason),
                        Second::Unreadable(why) => {
                            self.review_failure(&under, unreadable(&why), None)
                        }
                    },
                    StageReply::Fail(failure) => self.review_failure(&under, failure, None),
                    StageReply::Cancelled => self.review_cancelled(under.id, under.turn),
                    StageReply::Budget => self.budget_deny(under.id, under.turn),
                },
                First::Unreadable(why) => self.review_failure(&under, unreadable(&why), None),
            },
            StageReply::Fail(failure) => self.review_failure(&under, failure, None),
            StageReply::Cancelled => self.review_cancelled(id, turn),
            StageReply::Budget => self.budget_deny(id, turn),
        }
    }

    /// Asks one stage, and once more when its verdict does not read.
    fn ask_stage(
        &mut self,
        under: &UnderReview<'_>,
        endpoint: &ReviewEndpoint,
        shared: &str,
        stage: &str,
        max_output_tokens: Option<u64>,
        readable: fn(&str) -> Result<(), String>,
    ) -> Result<StageReply, Error> {
        let mut note = None;
        let mut why = String::new();
        for _ in 0..2 {
            if self.review_over_budget() {
                return Ok(StageReply::Budget);
            }
            let (conversation, previous) =
                self.review_conversation(under.id, under.effects, stage, note.as_deref());
            match self.send_review(
                under.turn,
                endpoint,
                shared,
                conversation,
                previous,
                max_output_tokens,
            )? {
                Ok(reply) => {
                    let text = reply.text();
                    match readable(&text) {
                        Ok(()) => return Ok(StageReply::Text(text)),
                        Err(unread) => {
                            why = unread;
                            note = Some(format!(
                                "Your reply could not be read: {why}. Reply in the form the \
                                 instructions give."
                            ));
                        }
                    }
                }
                Err(CallError::Failed { failure, .. }) => return Ok(StageReply::Fail(failure)),
                Err(CallError::Cancelled) => return Ok(StageReply::Cancelled),
            }
        }
        Ok(StageReply::Fail(unreadable(&why)))
    }

    /// A reviewer `allow`: the call runs.
    fn second_allow(
        &mut self,
        under: &UnderReview<'_>,
        endpoint: &ReviewEndpoint,
        stage: u8,
        reason: Option<String>,
    ) -> Result<Decided, Error> {
        self.consecutive = 0;
        let reviewer = Some(ReviewerRef {
            model: endpoint.reference.clone(),
            stage,
        });
        self.append(
            &Event::PermissionResolved(resolved(
                None,
                Decision::Allow,
                DecidedBy::Reviewer,
                reason,
                reviewer,
            )),
            under.turn,
            Some(under.id),
        )?;
        Ok(Ok((
            Arc::clone(under.tool),
            under.arguments.clone(),
            under.effects.declared.clone(),
        )))
    }

    /// A reviewer `block`: counted, then escalated or returned to the model.
    fn second_block(
        &mut self,
        under: &UnderReview<'_>,
        endpoint: &ReviewEndpoint,
        reason: String,
    ) -> Result<Decided, Error> {
        self.consecutive += 1;
        self.session_blocks += 1;
        let reviewer = Some(ReviewerRef {
            model: endpoint.reference.clone(),
            stage: 2,
        });
        let cause = if self.consecutive >= self.limits.consecutive {
            Escalation::ConsecutiveBlocks {
                reason: reason.clone(),
            }
        } else if self.session_blocks >= self.limits.session {
            Escalation::SessionBlocks {
                reason: reason.clone(),
            }
        } else {
            let completed = self.reviewer_deny(under.id, under.turn, None, reason, reviewer)?;
            return Ok(Err(completed));
        };
        self.escalate(under, cause, reason, reviewer)
    }

    /// A reviewer failure: counted and escalated. Never an allow.
    fn review_failure(
        &mut self,
        under: &UnderReview<'_>,
        failure: Failure,
        reviewer: Option<ReviewerRef>,
    ) -> Result<Decided, Error> {
        self.consecutive += 1;
        self.session_blocks += 1;
        let escalation = Escalation::ReviewerFailed {
            error: failure.clone(),
        };
        self.escalate(under, escalation, failure.message.clone(), reviewer)
    }

    /// Hands the call to a person, or denies it where no answer is possible.
    fn escalate(
        &mut self,
        under: &UnderReview<'_>,
        escalation: Escalation,
        reason: String,
        reviewer: Option<ReviewerRef>,
    ) -> Result<Decided, Error> {
        if !self.answerable {
            let completed = self.reviewer_deny(under.id, under.turn, None, reason, reviewer)?;
            return Ok(Err(completed));
        }
        match self.ask(
            under.id,
            under.turn,
            &under.call.name,
            &under.effects.declared,
            AskAbout::Review {
                escalation,
                offer: rule_offer(under.effects),
            },
        )? {
            Asked::Allow => {
                self.consecutive = 0;
                Ok(Ok((
                    Arc::clone(under.tool),
                    under.arguments.clone(),
                    under.effects.declared.clone(),
                )))
            }
            Asked::Deny(completed) => {
                self.consecutive = 0;
                Ok(Err(completed))
            }
            Asked::Gone(completed) => Ok(Err(completed)),
            Asked::Closed(request_id) => {
                let completed =
                    self.reviewer_deny(under.id, under.turn, Some(request_id), reason, reviewer)?;
                Ok(Err(completed))
            }
        }
    }

    /// The reviewer's request: the transcript cut after the call, then its
    /// effects, then the stage item. Both stages match up to the stage item.
    fn review_conversation(
        &mut self,
        id: &ActionId,
        effects: &Effects,
        stage: &str,
        note: Option<&str>,
    ) -> (Vec<Input>, Option<usize>) {
        let cut = self
            .reviewed
            .iter()
            .position(|item| item.action.as_ref() == Some(id))
            .map_or(self.reviewed.len(), |at| at + 1);
        let previous = self.reviewer_sent;
        self.reviewer_sent = Some(cut);
        let mut conversation: Vec<Input> = self
            .reviewed
            .iter()
            .take(cut)
            .map(|item| item.input.clone())
            .collect();
        let declared = serde_json::to_string(&effects.declared).unwrap_or_default();
        let workspace = self.workspace_label.clone();
        conversation.push(Input::User {
            text: format!("Declared effects: {declared}\nWorkspace root: {workspace}"),
        });
        conversation.push(Input::User {
            text: stage.to_owned(),
        });
        if let Some(note) = note {
            conversation.push(Input::User {
                text: note.to_owned(),
            });
        }
        (conversation, previous)
    }

    /// Sends one reviewer request and records its usage. A failed call
    /// writes no usage, and a reviewer reply is not streamed to watchers.
    fn send_review(
        &mut self,
        turn: &TurnId,
        endpoint: &ReviewEndpoint,
        shared: &str,
        conversation: Vec<Input>,
        previous_end: Option<usize>,
        max_output_tokens: Option<u64>,
    ) -> Result<Result<Reply, CallError>, Error> {
        let request = ModelRequest {
            system_prompt: shared.to_owned(),
            tools: Vec::new(),
            effort: None,
            tool_choice: "auto".to_owned(),
            cache_lifetime: CacheLifetime::OneHour,
            cache_key: self.reviewer_key.clone(),
            conversation,
            previous_end,
            max_output_tokens,
        };
        let reply = endpoint.provider.call(&request).run(&mut |_| {});
        if let Ok(reply) = &reply {
            let cost = endpoint
                .cost
                .as_ref()
                .map(|prices| crate::usage::price(prices, &reply.tokens));
            let recorded = UsageRecorded {
                generation_id: reply.generation_id.clone(),
                model: endpoint.reference.clone(),
                tokens: reply.tokens.clone(),
                web_searches: None,
                cost,
                subscription: endpoint.subscription.then_some(true),
                extension: None,
                origin_session_id: None,
            };
            self.append(&Event::UsageRecorded(recorded.clone()), turn, None)?;
            self.ledger.record(&recorded);
        }
        Ok(reply)
    }

    /// A cancelled review denies the call. It neither counts nor escalates.
    fn review_cancelled(&mut self, id: &ActionId, turn: &TurnId) -> Result<Decided, Error> {
        self.deny_quietly(
            id,
            turn,
            DecidedBy::Cancel,
            "The review was cancelled.",
            "cancelled",
        )
    }

    /// Denies the call as the reviewer, with no `permission_requested`: no
    /// answer is possible. Past the session limit, the turn ends `failed`
    /// with code `blocked` once the step's calls complete.
    fn reviewer_deny(
        &mut self,
        id: &ActionId,
        turn: &TurnId,
        request_id: Option<RequestId>,
        reason: String,
        reviewer: Option<ReviewerRef>,
    ) -> Result<Box<ToolCallCompleted>, Error> {
        self.append(
            &Event::PermissionResolved(resolved(
                request_id,
                Decision::Deny,
                DecidedBy::Reviewer,
                Some(reason.clone()),
                reviewer,
            )),
            turn,
            Some(id),
        )?;
        if self.session_blocks >= self.limits.session {
            self.turn_blocked = Some(Failure {
                code: ErrorCode::Blocked,
                message: format!(
                    "The reviewer blocked {} calls and no person can answer.",
                    self.session_blocks
                ),
                retry_after: None,
                provider: None,
            });
        }
        Ok(denied(
            "reviewer",
            format!(
                "The reviewer blocked this call: {reason} Respect this boundary and find \
                 another way to do the task. It did not run."
            ),
        ))
    }

    /// Denies the call without counting or escalating: the budget and the
    /// cancelled review go through here.
    fn deny_quietly(
        &mut self,
        id: &ActionId,
        turn: &TurnId,
        decided_by: DecidedBy,
        reason: &str,
        tool_reason: &str,
    ) -> Result<Decided, Error> {
        self.append(
            &Event::PermissionResolved(resolved(
                None,
                Decision::Deny,
                decided_by,
                Some(reason.to_owned()),
                None,
            )),
            turn,
            Some(id),
        )?;
        Ok(Err(denied(
            tool_reason,
            format!("{reason} It did not run."),
        )))
    }

    /// Whether billed spend has reached `budget.usd`.
    fn review_over_budget(&self) -> bool {
        self.budget
            .is_some_and(|limit| self.ledger.spend() >= limit)
    }

    /// Denies the call at the spending budget.
    fn budget_deny(&mut self, id: &ActionId, turn: &TurnId) -> Result<Decided, Error> {
        self.deny_quietly(
            id,
            turn,
            DecidedBy::Reviewer,
            "The session reached its spending budget.",
            "budget_exceeded",
        )
    }

    /// The `no_model` notice, once per session, before the escalation it
    /// explains.
    fn no_model_notice(&mut self, turn: &TurnId, failure: &Failure) -> Result<(), Error> {
        if failure.code == ErrorCode::NoModel && !self.no_model_noticed {
            self.no_model_noticed = true;
            self.append(
                &Event::Notice(Notice {
                    code: ErrorCode::NoModel,
                    message: "No reviewer model is set, so every reviewed call goes to a \
                              person. Set reviewer.model."
                        .to_owned(),
                    extension: None,
                }),
                turn,
                None,
            )?;
        }
        Ok(())
    }
}

/// A `permission_resolved` line: `request_id`, `decision`, `decided_by`,
/// `reason` and the deciding reviewer. Nothing else is ever set here: a
/// grant rides only a person's allow, feedback only a person's deny.
fn resolved(
    request_id: Option<RequestId>,
    decision: Decision,
    decided_by: DecidedBy,
    reason: Option<String>,
    reviewer: Option<ReviewerRef>,
) -> PermissionResolved {
    PermissionResolved {
        request_id,
        decision,
        decided_by,
        reason,
        feedback: None,
        grant: None,
        rule: None,
        reviewer,
    }
}

/// The rule an allow can remember: the call's subject and its prefix, absent
/// when no rule can match the call (`docs/permissions.md`, "What a rule
/// matches").
fn rule_offer(effects: &Effects) -> Option<RuleOffer> {
    let subject = effects.subject.as_ref()?;
    Some(RuleOffer {
        subject: subject.clone(),
        prefix: effects.prefix.clone().unwrap_or_else(|| subject.clone()),
    })
}

/// The escalation a verdict still unreadable on its second ask carries: a
/// model replied, but not in the format Fiber asked for.
fn unreadable(why: &str) -> Failure {
    Failure {
        code: ErrorCode::UnreadableReply,
        message: why.to_owned(),
        retry_after: None,
        provider: None,
    }
}
