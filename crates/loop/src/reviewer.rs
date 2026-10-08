//! Step 7 of the permission order (`docs/permissions.md`, "The reviewer"):
//! a call no earlier step decided is judged by a separate reviewer model.

use std::sync::Arc;

use contract::events::{
    AskStep, CacheLifetime, DecidedBy, Decision, Escalation, Event, Notice, PermissionResolved,
    ReviewerRef, RuleOffer, ToolCallCompleted, ToolCallRequested,
};
use contract::provider::{CallError, Cost, Input, ModelRequest, Provider, Reply};
use contract::shapes::Failure;
use contract::tool::{Effects, Tool};
use contract::{ActionId, ErrorCode, RequestId, TurnId};
use serde_json::{Map, Value};

use crate::calls::{Approved, Asked};
use crate::{Error, Loop, Model};

mod selection;
mod shown;

pub(crate) use shown::{Reviewed, render_reviewed};

/// What step 7 says about one call: it runs, or how its denial reads.
type Decided = Result<Approved, Box<ToolCallCompleted>>;

/// The first stage's output limit. A reviewer model that reasons before it
/// answers spends the limit on that reasoning, so the limit covers it as well
/// as the word. Measured against the models a first-party provider names for
/// review (`docs/model-routing.md`): `claude-sonnet-5-5` (and OpenRouter's
/// `anthropic/claude-sonnet-5.5`) takes 4 tokens for `allow` and 3 for `check`; `gpt-6-luna` and `gemini-3.8-flash` reasoned
/// for up to 50 and 82 tokens before the one-token word.
const FIRST_STAGE_OUTPUT_TOKENS: u64 = 128;

/// What a `no_model` escalation and notice say: nothing chose the
/// reviewer's model, so every reviewed call goes to a person
/// (`docs/permissions.md`, "How it runs").
pub const NO_MODEL_MESSAGE: &str =
    "No reviewer model is set, so every reviewed call goes to a person. Set reviewer.model.";

/// What judges a reviewed call (`docs/permissions.md`, "How it runs").
pub struct Reviewer {
    /// The provider the reviewer's requests go to.
    pub provider: Arc<dyn Provider>,
    /// The reviewer's model, and how its calls are priced.
    pub model: Model,
    /// The prompt-cache lifetime its requests ask for: `cache.lifetime`
    /// resolved for the reviewer's model (`docs/prompt-cache.md`, "Cache
    /// lifetime").
    pub cache_lifetime: CacheLifetime,
    /// The reviewer model's context window, in tokens.
    pub context_window: u64,
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

/// The reviewer's instructions (`docs/system-prompt.md`, "The texts").
pub(crate) struct Sections {
    /// What every reviewer request carries as its system prompt.
    pub shared: String,
    /// The first stage's instruction.
    pub first: String,
    /// The second stage's instruction.
    pub second: String,
    /// The handoff selection's instruction.
    pub handoff: String,
    /// The handoff selection's re-ask note, sent when a selection reply
    /// does not read.
    pub handoff_reask: String,
}

/// The reviewer's instructions, split per `docs/system-prompt.md`'s rule.
pub(crate) fn sections() -> Sections {
    let md = include_str!("../prompt/reviewer.md");
    Sections {
        shared: crate::prompt::section(md, "shared"),
        first: crate::prompt::section(md, "first-pass"),
        second: crate::prompt::section(md, "second-pass"),
        handoff: crate::prompt::section(md, "handoff"),
        handoff_reask: crate::prompt::section(md, "handoff-reask"),
    }
}

/// A reviewer request's system prompt (`docs/permissions.md`, "What the
/// person tells it"): the shared instructions alone where the person
/// wrote no notes, else the notes after them. One builder serves both
/// stages and the handoff selection, so every reviewer request shares
/// one system prompt.
fn system_prompt(shared: &str, notes: &str) -> String {
    if notes.trim().is_empty() {
        return shared.to_owned();
    }
    format!("{shared}\n\n{notes}")
}

/// What a first-stage verdict that read says.
#[derive(Debug)]
pub(crate) enum First {
    Allow,
    Check,
}

/// What a second-stage verdict that read says.
#[derive(Debug, PartialEq)]
pub(crate) enum Second {
    Allow { reason: Option<String> },
    Block { reason: String },
}

/// Reads a first-stage verdict: `check` or `allow`, cleaned.
pub(crate) fn read_first(text: &str) -> Result<First, String> {
    match clean(text).as_str() {
        "allow" => Ok(First::Allow),
        "check" => Ok(First::Check),
        _ => Err("expected one word, `check` or `allow`".to_owned()),
    }
}

/// Reads a second-stage verdict: `allow` or `block` plus a reason. The
/// verdict word may carry its separator, as in `allow: fine`. A `block`
/// with an empty reason does not read; an `allow` may have none.
pub(crate) fn read_second(text: &str) -> Result<Second, String> {
    let trimmed = text.trim_start();
    let mut words = trimmed.split_whitespace();
    let Some(first) = words.next() else {
        return Err("expected `allow` or `block` with a reason, but got nothing".to_owned());
    };
    let after = trimmed.strip_prefix(first).unwrap_or("");
    let (word, rest) = match first.split_once(':') {
        Some((head, tail)) => (head, [tail, after].concat()),
        None => (first, after.to_owned()),
    };
    let reason = rest
        .trim_start_matches(|c: char| c == ':' || c == '-' || c.is_whitespace())
        .trim_end()
        .to_owned();
    match clean(word).as_str() {
        "allow" => Ok(Second::Allow {
            reason: (!reason.is_empty()).then_some(reason),
        }),
        "block" if !reason.is_empty() => Ok(Second::Block { reason }),
        "block" => Err("a `block` needs a reason in one sentence, but got none".to_owned()),
        _ => Err("expected `allow` or `block` with a reason".to_owned()),
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
    cache_lifetime: CacheLifetime,
}

/// What one full stage (with its one re-ask) said.
enum StageReply<T> {
    Read(T),
    Fail(Failure),
    Cancelled,
    Budget,
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
                cache_lifetime: reviewer.cache_lifetime,
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
            Some(FIRST_STAGE_OUTPUT_TOKENS),
            read_first,
        )? {
            StageReply::Read(First::Allow) => self.second_allow(&under, &endpoint, 1, None),
            StageReply::Read(First::Check) => match self.ask_stage(
                &under,
                &endpoint,
                &prompt.shared,
                &prompt.second,
                None,
                read_second,
            )? {
                StageReply::Read(Second::Allow { reason }) => {
                    self.second_allow(&under, &endpoint, 2, reason)
                }
                StageReply::Read(Second::Block { reason }) => {
                    self.second_block(&under, &endpoint, reason)
                }
                StageReply::Fail(failure) => {
                    let reviewer = Some(ReviewerRef {
                        model: endpoint.reference.clone(),
                        stage: 2,
                    });
                    self.review_failure(&under, failure, reviewer)
                }
                StageReply::Cancelled => self.review_cancelled(under.id, under.turn),
                StageReply::Budget => self.budget_deny(under.id, under.turn),
            },
            StageReply::Fail(failure) => {
                let reviewer = Some(ReviewerRef {
                    model: endpoint.reference.clone(),
                    stage: 1,
                });
                self.review_failure(&under, failure, reviewer)
            }
            StageReply::Cancelled => self.review_cancelled(id, turn),
            StageReply::Budget => self.budget_deny(id, turn),
        }
    }

    /// Asks one stage, and once more when its verdict does not read.
    /// `parse` reads a verdict, or says what was wrong.
    fn ask_stage<T>(
        &mut self,
        under: &UnderReview<'_>,
        endpoint: &ReviewEndpoint,
        shared: &str,
        stage: &str,
        max_output_tokens: Option<u64>,
        parse: fn(&str) -> Result<T, String>,
    ) -> Result<StageReply<T>, Error> {
        let mut note = None;
        let mut why = String::new();
        let mut last = String::new();
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
                    match parse(&text) {
                        Ok(reading) => return Ok(StageReply::Read(reading)),
                        Err(unread) => {
                            why = unread;
                            last = text;
                            note = Some(format!(
                                "Your reply could not be read: {why}. Reply in the form the \
                                 instructions give."
                            ));
                        }
                    }
                }
                Err(CallError::Failed { failure, .. }) => return Ok(StageReply::Fail(failure)),
                Err(CallError::Cancelled { .. }) => return Ok(StageReply::Cancelled),
            }
        }
        // The escalation carries the last reply, quoted: a person's
        // escalation may show reviewer text that no reviewer request may.
        Ok(StageReply::Fail(unreadable(&format!(
            "{why}, but got {last:?}"
        ))))
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

    /// A reviewer failure: counted and escalated. Never an allow. `reviewer`
    /// names the model and the stage that failed; `None` when the reviewer
    /// could not be set up, and the denial is then `no_reviewer`.
    fn review_failure(
        &mut self,
        under: &UnderReview<'_>,
        failure: Failure,
        reviewer: Option<ReviewerRef>,
    ) -> Result<Decided, Error> {
        self.consecutive += 1;
        self.session_blocks += 1;
        let reason = failure.message.clone();
        let escalation = Escalation::ReviewerFailed { error: failure };
        self.escalate(under, escalation, reason, reviewer)
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
            AskStep::Review {
                escalation: Some(escalation),
                rule: rule_offer(under.effects),
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
            Asked::Gone(completed) => {
                // No answer is possible: the block budget still applies.
                self.check_block_end();
                Ok(Err(completed))
            }
            // A cancel is neither a reviewer allow nor a person's answer, so
            // the run of consecutive blocks stands (`docs/permissions.md`,
            // "What happens on a block").
            Asked::Cancelled => Ok(Err(self.cancelled_before_ran())),
            // The idle delay passed. `idle_left` is set; the caller writes
            // nothing for this call.
            Asked::Idle => Ok(Err(self.cancelled_before_ran())),
            // The close already denied the request by `cancel`: the block
            // budget still applies, and the call completes as the reviewer's
            // block (`docs/events.md`, `permission_resolved`).
            Asked::Closed => {
                self.check_block_end();
                Ok(Err(blocked(&reason)))
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
            .position(|item| matches!(&item.shown, shown::Shown::Call(call) if call == id))
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
            images: Vec::new(),
        });
        conversation.push(Input::User {
            text: stage.to_owned(),
            images: Vec::new(),
        });
        if let Some(note) = note {
            conversation.push(Input::User {
                text: note.to_owned(),
                images: Vec::new(),
            });
        }
        (conversation, previous)
    }

    /// Sends one reviewer request and records its usage. A call that ended
    /// without a reply writes what it saw, and a reviewer reply is not
    /// streamed to watchers.
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
            system_prompt: system_prompt(shared, &self.reviewer_notes),
            tools: Vec::new(),
            thinking: None,
            tool_choice: "auto".to_owned(),
            cache_lifetime: endpoint.cache_lifetime,
            cache_key: self.reviewer_key.clone(),
            conversation,
            previous_end,
            sent_tools: None,
            max_output_tokens,
            session_dir: self.log.dir().to_path_buf(),
        };
        let call = endpoint.provider.call(&request);
        let reply = crate::cancel::run_cancellable(&self.cancel, call, &mut |_| {});
        let (usage, inline) = match &reply {
            Ok(reply) => (reply.usage(), reply.cost),
            Err(error) => (error.usage().clone(), None),
        };
        let recorded = crate::usage::recorded(
            usage,
            inline,
            &endpoint.reference,
            endpoint.cost.as_ref(),
            endpoint.subscription,
        );
        let lookup = endpoint.provider.cost_lookup();
        self.write_usage(recorded, lookup, Some(turn), None)?;
        Ok(reply)
    }

    /// A cancelled review completes `cancelled`, not denied: its resolved
    /// line stays, decided by the cancel.
    fn review_cancelled(&mut self, id: &ActionId, turn: &TurnId) -> Result<Decided, Error> {
        self.append(
            &Event::PermissionResolved(resolved(
                None,
                Decision::Deny,
                DecidedBy::Cancel,
                Some("The review was cancelled.".to_owned()),
                None,
            )),
            turn,
            Some(id),
        )?;
        Ok(Err(self.cancelled_before_ran()))
    }

    /// Denies the call with no `permission_requested`: no answer is
    /// possible. With a reviewer it is the reviewer's block; with none, none
    /// could be set up, and the denial is `no_reviewer`. Past the session
    /// limit, the turn ends `failed` with code `blocked` once the step's
    /// calls complete.
    fn reviewer_deny(
        &mut self,
        id: &ActionId,
        turn: &TurnId,
        request_id: Option<RequestId>,
        reason: String,
        reviewer: Option<ReviewerRef>,
    ) -> Result<Box<ToolCallCompleted>, Error> {
        let decided_by = if reviewer.is_some() {
            DecidedBy::Reviewer
        } else {
            DecidedBy::NoReviewer
        };
        self.append(
            &Event::PermissionResolved(resolved(
                request_id,
                Decision::Deny,
                decided_by,
                Some(reason.clone()),
                reviewer,
            )),
            turn,
            Some(id),
        )?;
        self.check_block_end();
        Ok(blocked(&reason))
    }

    /// When the session limit is reached with no person to answer, the
    /// turn ends `failed` with code `blocked` once the step's calls
    /// complete.
    fn check_block_end(&mut self) {
        if self.session_blocks >= self.limits.session {
            self.turn_blocked = Some(Failure {
                code: ErrorCode::Blocked,
                message: format!(
                    "The reviewer blocked {} calls and no person can answer.",
                    self.session_blocks
                ),
                retry_after_ms: None,
                provider: None,
            });
        }
    }

    /// Denies the call without counting or escalating: the spending-budget
    /// denial goes through here.
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
        Ok(Err(crate::completion::denied(
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
            DecidedBy::Budget,
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
                    message: NO_MODEL_MESSAGE.to_owned(),
                    extension: None,
                }),
                turn,
                None,
            )?;
        }
        Ok(())
    }
}

/// The completion of a call the reviewer blocked: the reviewer's verdict on
/// `reason`, which never ran. What [`Loop::reviewer_deny`] completes a
/// denied call with, and what a `close` taken while an escalation waits
/// completes it with, the decision line already written.
fn blocked(reason: &str) -> Box<ToolCallCompleted> {
    crate::completion::denied(
        "reviewer",
        format!(
            "The reviewer blocked this call: {reason} Respect this boundary and find \
             another way to do the task. It did not run."
        ),
    )
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
    if effects.always_reviewed {
        return None;
    }
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
        retry_after_ms: None,
        provider: None,
    }
}
