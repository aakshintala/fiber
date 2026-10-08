//! The offer of the code a repository ships, before a process's first model
//! request (`docs/extensions.md`, "Code a repository ships"). With a person
//! to answer, the loop raises one `repository_code_offered` and waits for
//! the `reply` with `decisions`; with nobody, each unapproved item is
//! skipped with a notice, or a `required` one fails the run.

use std::sync::Arc;

use contract::commands::{Reply, ReplyAnswer};
use contract::events::{
    Event, Notice, OfferDecision, OfferedItem, OfferedKind, RepositoryCodeOffered,
    RepositoryCodeResolved,
};
use contract::inbox::{Ack, Delivery};
use contract::repository::{RepositoryCode, Unapproved};
use contract::shapes::Failure;
use contract::{ErrorCode, RequestId};

use crate::inbox::{self, InboxRecv, TurnInput};
use crate::{Error, Loop, mint};

/// An item's identity across offers: its kind, name and content hash.
type Key = (OfferedKind, String, String);

fn key(item: &OfferedItem) -> Key {
    (item.kind, item.name.clone(), item.hash.clone())
}

/// What the offer step knows: the seam, the session's skips, and whether
/// this process already ran the step.
#[derive(Default)]
pub(crate) struct State {
    /// The repository's code; `None` offers nothing.
    code: Option<Arc<dyn RepositoryCode>>,
    /// What the log held when the process started.
    folded: Folded,
    /// What a person decided in this process, approvals included.
    decided: Vec<Key>,
    /// The step ran in this process.
    done: bool,
}

impl State {
    /// The state a resumed process starts from.
    pub(crate) fn resumed(folded: Folded) -> Self {
        Self {
            folded,
            ..Self::default()
        }
    }
}

/// The offers a log holds: the items skipped for the session, and the
/// latest offer no `repository_code_resolved` answers.
#[derive(Default)]
pub(crate) struct Folded {
    /// Each item a resolved offer skipped.
    skipped: Vec<Key>,
    /// Offers not yet resolved, in log order; a re-raise replaces its
    /// earlier line.
    open: Vec<RepositoryCodeOffered>,
}

impl Folded {
    /// Folds one durable line.
    pub(crate) fn fold(&mut self, event: &Event) {
        if let Event::RepositoryCodeOffered(offered) = event {
            self.open.retain(|o| o.request_id != offered.request_id);
            self.open.push(offered.clone());
        } else if let Event::RepositoryCodeResolved(resolved) = event
            && let Some(at) = self
                .open
                .iter()
                .position(|o| o.request_id == resolved.request_id)
        {
            let offered = self.open.remove(at);
            for (item, decision) in offered.items.iter().zip(&resolved.decisions) {
                if *decision == OfferDecision::Skip {
                    self.skipped.push(key(item));
                }
            }
        }
    }

    fn pending(&self) -> Option<&RepositoryCodeOffered> {
        self.open.last()
    }
}

/// How a wait on an offer ended.
enum Waited {
    /// A person's reply resolved it.
    Resolved,
    /// `close` was taken: nobody can answer now.
    Closed,
    /// The idle delay passed, a shutdown started, or every sender is gone.
    Ended,
}

impl Loop {
    /// Offers the code `code` reads before the first model request
    /// (`docs/extensions.md`, "Code a repository ships"). Without it the
    /// loop offers nothing.
    pub fn repository_code(mut self, code: Arc<dyn RepositoryCode>) -> Self {
        self.repository.code = Some(code);
        self
    }

    /// Whether a person can answer an offer now: the loop is answerable and
    /// a `full` client is connected (`docs/extensions.md`, "Offering").
    fn can_answer(&self) -> bool {
        self.answerable && crate::status::clients_of(&Arc::downgrade(&self.log)) > 0
    }

    /// The step at the start of a resumed process that folded a pending
    /// offer: with a person to answer, it runs at once, so the offer is
    /// raised again before any prompt. `false` when the session ends while
    /// it waits.
    pub(crate) fn reraise_offer(&mut self) -> Result<bool, Error> {
        if self.repository.folded.pending().is_some() && self.can_answer() {
            self.offer(None)
        } else {
            Ok(true)
        }
    }

    /// Runs the offer step once per process; `false` when the session ends
    /// while an offer waits. `suspended` is the request id of the approval
    /// `finish_suspended` will raise again, if any: a reply to it is held
    /// for that wait.
    pub(crate) fn offer(&mut self, suspended: Option<&RequestId>) -> Result<bool, Error> {
        if self.repository.done {
            return Ok(true);
        }
        self.repository.done = true;
        let Some(code) = self.repository.code.clone() else {
            return Ok(true);
        };
        // A pending offer is raised again verbatim, same `request_id`, only
        // when someone can answer it; otherwise it stays in the log.
        if let Some(pending) = self.repository.folded.pending().cloned()
            && self.can_answer()
            && let Waited::Ended = self.wait_offer(code.as_ref(), &pending, suspended)?
        {
            return Ok(false);
        }
        // Each round offers only what is still undecided, so content that
        // changed under an answer is offered again at once.
        loop {
            let unapproved = code.unapproved().map_err(Error::RepositoryCode)?;
            if !self.can_answer() {
                self.nobody_to_ask(&unapproved)?;
                return Ok(true);
            }
            let items: Vec<OfferedItem> = unapproved
                .into_iter()
                .filter(|u| !u.never && !self.offer_answered(&u.offered))
                .map(|u| u.offered)
                .collect();
            if items.is_empty() {
                return Ok(true);
            }
            let offer = RepositoryCodeOffered {
                request_id: RequestId(mint("r_")),
                items,
            };
            if let Waited::Ended = self.wait_offer(code.as_ref(), &offer, suspended)? {
                return Ok(false);
            }
        }
    }

    /// Whether `item`, with this content, was skipped earlier in the
    /// session or answered in this process.
    fn offer_answered(&self, item: &OfferedItem) -> bool {
        let key = key(item);
        self.repository.folded.skipped.contains(&key) || self.repository.decided.contains(&key)
    }

    /// With nobody to ask: a notice for each item not `required`, in order,
    /// then, when any is `required`, the run fails naming every required
    /// one, with the first one's code.
    fn nobody_to_ask(&self, unapproved: &[Unapproved]) -> Result<(), Error> {
        let mut required = Vec::new();
        for item in unapproved.iter().map(|u| &u.offered) {
            if item.required {
                required.push(item);
                continue;
            }
            let message = format!(
                "The repository declares the {} `{}`, which nobody approved: it was not loaded. Run `fiber approve` in the repository to approve it.",
                kind_words(item.kind),
                item.name
            );
            self.log.append(
                &Event::Notice(Notice {
                    code: ErrorCode::RepositoryCodeSkipped,
                    message,
                    extension: None,
                }),
                None,
                None,
            )?;
        }
        let Some(first) = required.first() else {
            return Ok(());
        };
        let names: Vec<String> = required
            .iter()
            .map(|item| format!("the {} `{}`", kind_words(item.kind), item.name))
            .collect();
        let them = if required.len() == 1 { "it" } else { "them" };
        Err(Error::RepositoryCode(Failure {
            code: unapproved_code(first.kind),
            message: format!(
                "The repository requires {}, which nobody approved, and nobody could be asked. Run `fiber approve` in the repository to approve {them}.",
                names.join(" and ")
            ),
            retry_after_ms: None,
            provider: None,
        }))
    }

    /// Writes `offer` and waits for its answer. Waiting is idle: the idle
    /// delay counts from the start of the wait (`docs/invocation.md`,
    /// "Lifecycle"). Prompts and other turn input wait in `deferred`;
    /// news is written at once; a reply naming `suspended` is held for its
    /// own wait.
    fn wait_offer(
        &mut self,
        code: &dyn RepositoryCode,
        offer: &RepositoryCodeOffered,
        suspended: Option<&RequestId>,
    ) -> Result<Waited, Error> {
        self.log
            .append(&Event::RepositoryCodeOffered(offer.clone()), None, None)?;
        let deadline = self.idle_deadline();
        loop {
            let delivery = match self.take_held_offer_answer(&offer.request_id) {
                Some(delivery) => delivery,
                None => match self.recv_until(deadline, false, None) {
                    InboxRecv::Delivery(delivery) => delivery,
                    InboxRecv::Idle
                    | InboxRecv::Closed
                    | InboxRecv::Unattended
                    | InboxRecv::Warm => return Ok(Waited::Ended),
                },
            };
            match delivery {
                Delivery::Reply(reply, ack) if reply.request_id == offer.request_id => {
                    if self.answer_offer(code, offer, reply, ack)? {
                        return Ok(Waited::Resolved);
                    }
                }
                Delivery::Reply(reply, ack) if Some(&reply.request_id) == suspended => {
                    self.deferred.push_back(Delivery::Reply(reply, ack));
                }
                Delivery::Close(ack) => {
                    self.take_close(ack);
                    return Ok(Waited::Closed);
                }
                // A stale wake from an earlier turn's cancel: no turn runs.
                Delivery::Cancelled => {}
                held @ (Delivery::Prompt(..)
                | Delivery::Steer(..)
                | Delivery::SteerDrop(..)
                | Delivery::Handoff(..)
                | Delivery::Model(..)
                | Delivery::Credential(..)
                | Delivery::Rewind(..)
                | Delivery::Job(_)
                | Delivery::JobLine(_)) => self.deferred.push_back(held),
                now @ (Delivery::Reply(..)
                | Delivery::ExtensionExec(_)
                | Delivery::ExtensionLog(_)
                | Delivery::Interaction(_)
                | Delivery::Resolved(..)) => {
                    self.admit_idle(now, &mut TurnInput::of(Vec::new()))?;
                }
            }
        }
    }

    /// The first held delivery that answers the offer `request_id`: a
    /// `reply` naming it, or `close`. Everything else stays held.
    fn take_held_offer_answer(&mut self, request_id: &RequestId) -> Option<Delivery> {
        let at = self.deferred.iter().position(|held| match held {
            Delivery::Reply(reply, _) => reply.request_id == *request_id,
            Delivery::Close(_) => true,
            Delivery::Prompt(..)
            | Delivery::Steer(..)
            | Delivery::SteerDrop(..)
            | Delivery::Handoff(..)
            | Delivery::Model(..)
            | Delivery::Credential(..)
            | Delivery::Rewind(..)
            | Delivery::Interaction(_)
            | Delivery::Resolved(..)
            | Delivery::Job(_)
            | Delivery::JobLine(_)
            | Delivery::ExtensionExec(_)
            | Delivery::ExtensionLog(_)
            | Delivery::Cancelled => false,
        })?;
        self.deferred.remove(at)
    }

    /// Applies a reply to `offer`: `true` once `repository_code_resolved`
    /// is written and the reply accepted. A reply that does not fit, or a
    /// decision the seam fails to record, is rejected and the offer stays
    /// pending; decisions recorded before the failure stay recorded.
    fn answer_offer(
        &mut self,
        code: &dyn RepositoryCode,
        offer: &RepositoryCodeOffered,
        reply: Reply,
        ack: Ack,
    ) -> Result<bool, Error> {
        let ReplyAnswer::Decisions { decisions } = reply.answer else {
            inbox::reject(ack, ErrorCode::InvalidArguments, inbox::UNFIT_REPLY);
            return Ok(false);
        };
        if decisions.len() != offer.items.len() {
            inbox::reject(ack, ErrorCode::InvalidArguments, inbox::UNFIT_REPLY);
            return Ok(false);
        }
        for (item, decision) in offer.items.iter().zip(&decisions) {
            // A skip lives only in the log and this session.
            if *decision == OfferDecision::Skip {
                continue;
            }
            if let Err(failure) = code.decide(item, *decision) {
                inbox::reject(ack, failure.code, &failure.message);
                return Ok(false);
            }
        }
        self.log.append(
            &Event::RepositoryCodeResolved(RepositoryCodeResolved {
                request_id: offer.request_id.clone(),
                decisions,
            }),
            None,
            None,
        )?;
        self.repository.decided.extend(offer.items.iter().map(key));
        inbox::accept(ack);
        Ok(true)
    }
}

/// How a message names a kind.
fn kind_words(kind: OfferedKind) -> &'static str {
    match kind {
        OfferedKind::Extension => "extension",
        OfferedKind::Hook => "hook",
        OfferedKind::McpServer => "MCP server",
    }
}

/// The failure for a `required` item nobody approved, with nobody to ask.
fn unapproved_code(kind: OfferedKind) -> ErrorCode {
    match kind {
        OfferedKind::Extension => ErrorCode::ExtensionUnapproved,
        OfferedKind::Hook => ErrorCode::HookUnapproved,
        OfferedKind::McpServer => ErrorCode::McpServerUnapproved,
    }
}
