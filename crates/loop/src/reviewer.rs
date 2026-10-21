//! Step 7 of the permission order (`docs/permissions.md`, "The reviewer"):
//! a call no earlier step decided is judged by a separate reviewer model.

use std::sync::Arc;
use std::thread;

use contract::diag::{DebugLog, Observe};
use contract::events::{
    AskStep, CacheLifetime, DecidedBy, Decision, Escalation, Event, Notice, PermissionResolved,
    ReviewedCall, ReviewerRef, ReviewerUse, RuleOffer, ToolCallCompleted, ToolCallRequested,
};
use contract::provider::{CallError, Cost, Input, ModelCall, ModelRequest, Provider, Reply};
use contract::shapes::Failure;
use contract::tool::Effects;
use contract::{ActionId, ErrorCode, RequestId, TurnId};

use crate::before_tool::Prepared;
use crate::calls::{Asked, Decided};
use crate::cancel::TurnCancel;
use crate::{Error, Loop, Model};

mod request;
mod selection;
mod shown;

pub(crate) use shown::summary_turn;
/// What the reviewer is shown, and how the loop renders it
/// (`docs/permissions.md`, "What it is shown"). Public for the `review`
/// jig's replay, which folds a source log through the same renderer
/// (`docs/testing.md`, "Jigs").
pub use shown::{Reviewed, Shown, render_reviewed, summary_skips};

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
    /// The thinking levels the reviewer's model declares (`docs/model-routing.md`, "Thinking").
    /// Every reviewer request asks for the lowest; none when it declares none.
    pub thinking_levels: Vec<contract::ThinkingLevel>,
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
/// Shared by the serial ask and the batch build, so both judge the same call.
pub(crate) struct UnderReview<'a> {
    pub(crate) call: &'a ToolCallRequested,
    pub(crate) id: &'a ActionId,
    pub(crate) turn: &'a TurnId,
    pub(crate) prepared: &'a Prepared,
}

/// What sends a reviewer request: the reviewer's provider and model, cloned
/// off the loop so a request borrows nothing while it runs.
pub(crate) struct ReviewEndpoint {
    /// The reviewer's provider: the only handle a stage-1 worker holds.
    pub(crate) provider: Arc<dyn Provider>,
    /// The reviewer's model reference, carried on the usage line.
    pub(crate) reference: String,
    /// The reviewer's model prices, carried on the usage line.
    pub(crate) cost: Option<Cost>,
    /// Whether a subscription serves the reviewer.
    pub(crate) subscription: bool,
    /// The prompt-cache lifetime its requests ask for.
    pub(crate) cache_lifetime: CacheLifetime,
    /// The thinking level every reviewer request asks for.
    pub(crate) thinking: Option<contract::ThinkingLevel>,
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
    ///
    /// Public for the `review` jig's replay, which replays a source log's
    /// recorded calls through the same judge (`docs/testing.md`, "Jigs").
    pub fn review(
        &mut self,
        call: &ToolCallRequested,
        id: &ActionId,
        turn: &TurnId,
        prepared: Prepared,
    ) -> Result<Decided, Error> {
        let prompt = sections();
        if self.review_over_budget() {
            return self.budget_deny(id, turn);
        }
        let under = UnderReview {
            call,
            id,
            turn,
            prepared: &prepared,
        };
        let endpoint = match &self.reviewer {
            Ok(reviewer) => ReviewEndpoint {
                provider: Arc::clone(&reviewer.provider),
                reference: reviewer.model.reference.clone(),
                cost: reviewer.model.cost.clone(),
                subscription: reviewer.model.subscription,
                cache_lifetime: reviewer.cache_lifetime,
                thinking: request::lowest(&reviewer.thinking_levels),
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
            |id| ReviewerUse::Stage1 { action_id: id },
            Some(request::FIRST_STAGE_OUTPUT_TOKENS),
            read_first,
        )? {
            StageReply::Read(First::Allow) => self.second_allow(&under, &endpoint, 1, None),
            StageReply::Read(First::Check) => match self.ask_stage(
                &under,
                &endpoint,
                &prompt.shared,
                &prompt.second,
                |id| ReviewerUse::Stage2 { action_id: id },
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
    #[allow(
        clippy::too_many_arguments,
        reason = "the call under review, the endpoint, the prompts, how the stage marks its usage, the cap, and the verdict reader are one ask"
    )]
    fn ask_stage<T>(
        &mut self,
        under: &UnderReview<'_>,
        endpoint: &ReviewEndpoint,
        shared: &str,
        stage: &str,
        make_reviewer: fn(ActionId) -> ReviewerUse,
        max_output_tokens: Option<u64>,
        parse: fn(&str) -> Result<T, String>,
    ) -> Result<StageReply<T>, Error> {
        let reviewer = make_reviewer(under.id.clone());
        let mut note = None;
        let mut why = String::new();
        let mut last = String::new();
        // A budget already reached admits no stage and times nothing.
        if self.review_over_budget() {
            return Ok(StageReply::Budget);
        }
        // One entered stage's timer, with the session's registered name
        // for the call under review. The public replay entry built
        // outside the registry has no validated name and times nothing.
        // The registry lookup is diagnostic-only: at `info` the cost
        // stays one level check, with no lookup, clone or allocation.
        let _stage = self
            .diag
            .writer()
            .debugging()
            .then(|| {
                self.tools
                    .get_key_value(under.call.name.as_str())
                    .and_then(|(registered, _)| {
                        self.diag
                            .timed_tool(contract::diag::Op::ToolReviewer, registered)
                    })
            })
            .flatten();
        for _ in 0..2 {
            if self.review_over_budget() {
                return Ok(StageReply::Budget);
            }
            let (conversation, previous) = self.review_conversation(under, stage, note.as_deref());
            match self.send_review(
                under.turn,
                endpoint,
                reviewer.clone(),
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
        let mut line = resolved(None, Decision::Allow, DecidedBy::Reviewer, reason, reviewer);
        line.call = Some(reviewed_call(under));
        line.hosts = under.prepared.effects.hosts.clone();
        self.append(&Event::PermissionResolved(line), under.turn, Some(under.id))?;
        Ok(Ok(under.prepared.clone()))
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
            let completed = self.reviewer_deny(under, reason, reviewer)?;
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
            let completed = self.reviewer_deny(under, reason, reviewer)?;
            return Ok(Err(completed));
        }
        match self.ask(
            under.id,
            under.turn,
            &under.call.name,
            &under.prepared.effects,
            AskStep::Review {
                escalation: Some(escalation),
                rule: rule_offer(&under.prepared.effects),
            },
        )? {
            Asked::Allow => {
                self.consecutive = 0;
                Ok(Ok(under.prepared.clone()))
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

    /// The reviewer's conversation: the transcript cut after the call, then
    /// its effects, then the stage item. Moves the shared cursor past the
    /// cut; the batch builds through `cut_for` and `stage_conversation`
    /// instead, chaining a local cursor so the shared one moves once.
    /// Visible for the batch-build unit test's serial oracle.
    pub(crate) fn review_conversation(
        &mut self,
        under: &UnderReview<'_>,
        stage: &str,
        note: Option<&str>,
    ) -> (Vec<Input>, Option<usize>) {
        let cut = cut_for(&self.reviewed, under.id);
        let previous = self.reviewer_sent;
        self.reviewer_sent = Some(cut);
        let item = under_item(under.prepared, &under.call.name, &self.workspace_label);
        (
            stage_conversation(&self.reviewed, cut, item, stage, note),
            previous,
        )
    }

    /// Builds one first-stage request without moving the shared cursor:
    /// `sent` is the cursor the build chains from, and `cut` is the value
    /// the shared cursor holds after it. The batch builds every call's in
    /// request order over the fixed `reviewed` vec, so the bytes match the
    /// serial path's; only the provider round trips overlap.
    #[allow(
        clippy::too_many_arguments,
        reason = "the call under review, the endpoint and prompts, the note, and the chained cursor are one build"
    )]
    pub(crate) fn build_stage1(
        &self,
        under: &UnderReview<'_>,
        endpoint: &ReviewEndpoint,
        shared: &str,
        stage: &str,
        note: Option<&str>,
        sent: Option<usize>,
    ) -> BuiltStage1 {
        let cut = cut_for(&self.reviewed, under.id);
        let item = under_item(under.prepared, &under.call.name, &self.workspace_label);
        let conversation = stage_conversation(&self.reviewed, cut, item, stage, note);
        let request = self.review_request(
            endpoint,
            shared,
            conversation,
            sent,
            Some(request::FIRST_STAGE_OUTPUT_TOKENS),
        );
        BuiltStage1 {
            request,
            reviewer_use: ReviewerUse::Stage1 {
                action_id: under.id.clone(),
            },
            cut,
        }
    }
    /// Sends one reviewer request and records its usage, marked with what
    /// the reviewer was doing, so the spend ties to the call under review.
    /// A call that ended without a reply writes what it saw, and a reviewer
    /// reply is not streamed to watchers.
    #[allow(
        clippy::too_many_arguments,
        reason = "the turn, the endpoint, what the reviewer was doing, and the request are one send"
    )]
    fn send_review(
        &mut self,
        turn: &TurnId,
        endpoint: &ReviewEndpoint,
        reviewer: ReviewerUse,
        shared: &str,
        conversation: Vec<Input>,
        previous_end: Option<usize>,
        max_output_tokens: Option<u64>,
    ) -> Result<Result<Reply, CallError>, Error> {
        let request = self.review_request(
            endpoint,
            shared,
            conversation,
            previous_end,
            max_output_tokens,
        );
        let call = start_review_call(&endpoint.provider, self.reviewer_observe.as_ref(), &request);
        let reply = crate::cancel::run_cancellable(&self.cancel, call, &mut |_| {});
        self.record_review_usage(&reply, endpoint, reviewer, turn)?;
        Ok(reply)
    }

    /// One reviewer request's bytes: the shared instructions with the
    /// person's notes, the stage's conversation, and the cursor the cache
    /// chain extends from. One builder serves the serial send and every
    /// batch request, so the two match byte for byte.
    fn review_request(
        &self,
        endpoint: &ReviewEndpoint,
        shared: &str,
        conversation: Vec<Input>,
        previous_end: Option<usize>,
        max_output_tokens: Option<u64>,
    ) -> ModelRequest {
        ModelRequest {
            system_prompt: system_prompt(shared, &self.reviewer_notes),
            tools: Vec::new(),
            thinking: endpoint.thinking,
            tool_choice: "auto".to_owned(),
            cache_lifetime: endpoint.cache_lifetime,
            cache_key: self.reviewer_key.clone(),
            conversation,
            previous_end,
            sent_tools: None,
            max_output_tokens,
            session_dir: self.log.dir().to_path_buf(),
        }
    }

    /// Records one reviewer reply's usage, marked with what the reviewer
    /// was doing, so the spend ties to the call under review. The serial
    /// send and the batch write-back share it, so a fanned-out request
    /// records the same line the serial path would.
    fn record_review_usage(
        &mut self,
        reply: &Result<Reply, CallError>,
        endpoint: &ReviewEndpoint,
        reviewer: ReviewerUse,
        turn: &TurnId,
    ) -> Result<(), Error> {
        let (usage, inline) = match reply {
            Ok(reply) => (reply.usage(), reply.cost),
            Err(error) => (error.usage().clone(), None),
        };
        let mut recorded = crate::usage::recorded(
            usage,
            inline,
            &endpoint.reference,
            endpoint.cost.as_ref(),
            endpoint.subscription,
        );
        recorded.line.reviewer = Some(reviewer);
        let lookup = endpoint.provider.cost_lookup();
        self.write_usage(recorded, lookup, Some(turn), None)
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
        under: &UnderReview<'_>,
        reason: String,
        reviewer: Option<ReviewerRef>,
    ) -> Result<Box<ToolCallCompleted>, Error> {
        let decided_by = if reviewer.is_some() {
            DecidedBy::Reviewer
        } else {
            DecidedBy::NoReviewer
        };
        let mut line = resolved(
            None,
            Decision::Deny,
            decided_by,
            Some(reason.clone()),
            reviewer,
        );
        line.call = Some(reviewed_call(under));
        self.append(&Event::PermissionResolved(line), under.turn, Some(under.id))?;
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

/// A first-stage request built for one call: the request itself, what the
/// reviewer was doing for its usage line, and where `reviewed` cut for it.
/// `cut` is both the stage-2 rebuild point and the cursor value after it.
pub(crate) struct BuiltStage1 {
    /// The request as the serial path would send it.
    pub(crate) request: ModelRequest,
    /// What the reviewer was doing, for the usage line.
    pub(crate) reviewer_use: ReviewerUse,
    /// Where `reviewed` cut for the call.
    pub(crate) cut: usize,
}

/// What a first-stage worker carries across the thread boundary: cloned
/// `Arc`s and, per spawn, an owned request. No `&mut Loop`, no log handle
/// and no rules guard cross, so the deciding thread stays the only writer.
#[derive(Clone)]
pub(crate) struct Stage1Worker {
    /// The reviewer's provider: the only handle a worker holds.
    pub(crate) provider: Arc<dyn Provider>,
    /// The observer of the reviewer's calls, when one is installed.
    pub(crate) observe: Option<Observe>,
    /// The turn's cancel, so a cancelled turn wakes every worker.
    pub(crate) cancel: Arc<TurnCancel>,
}

/// Sends prebuilt first-stage requests, one thread each inside a
/// `thread::scope`, and returns every reply in request order. A worker only
/// runs its call to its end; the caller records usage and applies verdicts
/// in order after the join.
pub(crate) fn fanout_stage1(
    worker: &Stage1Worker,
    requests: Vec<ModelRequest>,
) -> Vec<Result<Reply, CallError>> {
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(requests.len());
        for request in requests {
            let worker = worker.clone();
            handles.push(scope.spawn(
                #[allow(
                    clippy::result_large_err,
                    reason = "the error is the reviewer call's, returned unchanged"
                )]
                move || run_stage1(&worker, request),
            ));
        }
        let mut replies = Vec::with_capacity(handles.len());
        for handle in handles {
            // A panicking provider is a bug: unwinding carries its panic out
            // of the scope, as a serial send would unwind the deciding thread.
            replies.push(match handle.join() {
                Ok(reply) => reply,
                Err(payload) => std::panic::resume_unwind(payload),
            });
        }
        replies
    })
}

/// Runs one prebuilt first-stage request to its end: the reviewer's own
/// factory builds the call, and a cancel wakes it. The caller records usage
/// and applies the verdict.
#[allow(
    clippy::result_large_err,
    reason = "the error is the reviewer call's, returned unchanged"
)]
fn run_stage1(worker: &Stage1Worker, request: ModelRequest) -> Result<Reply, CallError> {
    let call = start_review_call(&worker.provider, worker.observe.as_ref(), &request);
    crate::cancel::run_cancellable(&worker.cancel, call, &mut |_| {})
}

/// Where `reviewed` cuts for the call `id`: after its call item, or the end
/// when the call is not shown.
fn cut_for(reviewed: &[Reviewed], id: &ActionId) -> usize {
    reviewed
        .iter()
        .position(|item| matches!(&item.shown, Shown::Call(call) if call == id))
        .map_or(reviewed.len(), |at| at + 1)
}

/// The "Call under review" item for a call: its declared effects, or its
/// rewrite, with the workspace root. Both stages share it, so they match up
/// to the stage item. A rewritten call is judged as the call that will run:
/// its effects item names the rewritten call's declared effects and the
/// arguments it runs with.
fn under_item(prepared: &Prepared, name: &str, workspace: &str) -> Input {
    // The call stays where it ran: the item after it names it, so the
    // reviewer never counts on it coming last.
    let declared = serde_json::to_string(&prepared.effects.declared).unwrap_or_default();
    let effects = match &prepared.rewritten {
        Some(changed_by) => {
            let names = changed_by.join(", ");
            let arguments = serde_json::to_string(&prepared.arguments).unwrap_or_default();
            format!(
                "Call under review: the last tool call above, {name}.\nDeclared effects: {declared}\nExtension {names} rewrote this call's arguments; it runs with {arguments}\nWorkspace root: {workspace}"
            )
        }
        None => format!(
            "Call under review: the last tool call above, {name}.\nDeclared effects: {declared}\nWorkspace root: {workspace}"
        ),
    };
    Input::User {
        text: effects,
        images: Vec::new(),
    }
}

/// One stage's conversation over `reviewed` cut at `cut`: the transcript to
/// the cut, then the call's effects item, then the stage item and the re-ask
/// note. Both stages match up to the stage item. The history never changes
/// after a line renders.
fn stage_conversation(
    reviewed: &[Reviewed],
    cut: usize,
    item: Input,
    stage: &str,
    note: Option<&str>,
) -> Vec<Input> {
    let mut conversation: Vec<Input> = reviewed
        .iter()
        .take(cut)
        .map(|item| item.input.clone())
        .collect();
    conversation.push(item);
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
    conversation
}

/// Starts one reviewer request: the reviewer's own factory builds the call
/// at the first attempt, never the session's. Every review stage and the
/// handoff selection share this one start; the caller records usage.
fn start_review_call(
    provider: &Arc<dyn Provider>,
    observe: Option<&Observe>,
    request: &ModelRequest,
) -> Box<dyn ModelCall> {
    match observe {
        Some(observe) => observe(request, std::num::NonZeroU32::MIN),
        None => provider.call(request),
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

/// The reviewed call's declared effects and rewrite, for a decision that
/// raised no request: what the reviewer judged, so the `review` jig can
/// replay a call that never started.
fn reviewed_call(under: &UnderReview<'_>) -> ReviewedCall {
    let (arguments, changed_by) = under.prepared.rewrite();
    ReviewedCall {
        declared: under.prepared.effects.declared.clone(),
        always_reviewed: under.prepared.effects.always_reviewed,
        arguments,
        changed_by,
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
        call: None,
        request_id,
        decision,
        decided_by,
        reason,
        feedback: None,
        grant: None,
        rule: None,
        reviewer,
        hosts: None,
    }
}

/// The rule an allow can remember: the call's subject and its prefix, absent
/// when no rule can match the call (`docs/permissions.md`, "What a rule
/// matches").
pub(crate) fn rule_offer(effects: &Effects) -> Option<RuleOffer> {
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
