//! The offer of the code a repository ships, before a process's first model
//! request (`docs/extensions.md`, "Code a repository ships"). With a person
//! to answer, the loop raises one `repository_code_offered` and waits for
//! the `reply` with `decisions`; with nobody, each unapproved item is
//! skipped with a notice, or a `required` one fails the run.

use std::sync::Arc;

use contract::commands::{Reply, ReplyAnswer};
use contract::events::{
    Event, Notice, OfferDecision, OfferedItem, OfferedKind, RepositoryCodeOffered,
    RepositoryCodeResolved, ToolReplaced,
};
use contract::inbox::{Ack, Delivery};
use contract::repository::{Decided, RepositoryCode, Unapproved};
use contract::shapes::Failure;
use contract::tool::Tool;
use contract::{ErrorCode, RequestId};

use crate::inbox::{self, InboxRecv, TurnInput};
use crate::{Error, Loop, mint};

/// An item's identity across offers: its kind, name and content hash.
type Key = (OfferedKind, String, String);

fn key(item: &OfferedItem) -> Key {
    (item.kind, item.name.clone(), item.hash.clone())
}

/// What the session loads once its offer step has run, before its first
/// preamble: the repository code approved since the process started
/// (`docs/extensions.md`, "Offering").
pub type LoadApproved = Arc<dyn Fn(&OfferOutcome) -> Result<Approved, Failure> + Send + Sync>;

/// What the post-offer step loaded: each tool with who registered it,
/// and the durable lines written before `preamble_built`.
#[derive(Default)]
pub struct Approved {
    /// `(registered_by, tool)` pairs for the loop.
    pub tools: Vec<(String, Arc<dyn Tool>)>,
    /// Durable events, written in order before `preamble_built`.
    pub events: Vec<Event>,
}

/// One content, as `offer.rs`'s private `Key` names it: its kind, name
/// and content hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferKey {
    /// What it is.
    pub kind: OfferedKind,
    /// Its name.
    pub name: String,
    /// Its content hash.
    pub hash: String,
}

impl OfferKey {
    fn of(key: &Key) -> Self {
        Self {
            kind: key.0,
            name: key.1.clone(),
            hash: key.2.clone(),
        }
    }
}

/// What the offer step settled: every content this session skipped, and
/// every content it settled or wrote a line for. Built after `offer`
/// returned `Ok(true)`, so it holds every answer of this process's offer
/// step.
#[derive(Debug, Clone, Default)]
pub struct OfferOutcome {
    /// Every content this session skipped: the log's folded skips plus
    /// this process's `skip` decisions.
    pub skipped: Vec<OfferKey>,
    /// Every content the offer step settled or wrote a line for:
    /// `skipped`, plus this process's decisions and notices, where the
    /// notices include the `never` items the round filtered.
    pub reported: Vec<OfferKey>,
    /// The tool names registered before the post-offer load: what was
    /// registered stands, and the load declares none of them again.
    pub held: Vec<String>,
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
    /// This process's `skip` decisions: content never started in it, even
    /// when approved meanwhile by `fiber approve` or another session.
    skipped_now: Vec<Key>,
    /// The keys `nobody_to_ask` wrote a notice for, and the `never` keys
    /// the offer round filtered out without offering, required ones
    /// included: content the load counts as reported.
    noted: Vec<Key>,
    /// What the session loads once its offer step has run, before its
    /// first preamble.
    pub(crate) load: Option<LoadApproved>,
    /// The load ran in this process.
    pub(crate) loaded: bool,
    /// Prepares the session's extensions again once this process's offer
    /// recorded an approval of an extension, before its first preamble.
    pub(crate) late: Option<crate::PrepareReload>,
    /// This process's offer recorded an approval of an extension. The
    /// late step runs once, then clears it.
    pub(crate) approved_extension: bool,
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
            let mut items = Vec::new();
            for u in unapproved {
                // Content the person never wants is not offered again, but
                // it counts as reported: the post-offer load drops it with
                // no line.
                if u.never && !self.offer_answered(&u.offered) {
                    self.repository.noted.push(key(&u.offered));
                    continue;
                }
                if !self.offer_answered(&u.offered) {
                    items.push(u.offered);
                }
            }
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
    /// one, with the first one's code. Content this session already
    /// answered, inherited or in this process, is left out: a skipped
    /// item stays skipped with no second notice, and never fails the run.
    fn nobody_to_ask(&mut self, unapproved: &[Unapproved]) -> Result<(), Error> {
        let fresh: Vec<&OfferedItem> = unapproved
            .iter()
            .map(|u| &u.offered)
            .filter(|item| !self.offer_answered(item))
            .collect();
        let mut required = Vec::new();
        for item in fresh {
            if item.required {
                required.push(item);
                continue;
            }
            let message = format!(
                "The repository declares the {} `{}`, which nobody approved: it was not loaded. Run `fiber approve` in the repository to approve it.",
                kind_words(item.kind),
                item.name
            );
            self.repository.noted.push(key(item));
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
        // A held session message dies with the failed run
        // (`docs/tools.md`, "Messaging other sessions").
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
            let delivery = match self.take_held_answer(&offer.request_id) {
                Some(delivery) => delivery,
                None => match self.recv_until(deadline, false, None)? {
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
                | Delivery::Message(..)
                | Delivery::Handoff(..)
                | Delivery::Model(..)
                | Delivery::Reload(..)
                | Delivery::McpReconnect(..)
                | Delivery::Credential(..)
                | Delivery::Rewind(..)
                | Delivery::Job(_)
                | Delivery::JobLine(_)
                | Delivery::ProposeNotes(..)
                | Delivery::InnerCall(_)) => self.deferred.push_back(held),
                now @ (Delivery::Reply(..)
                | Delivery::Name(..)
                | Delivery::Shell(..)
                | Delivery::ExtensionExec(_)
                | Delivery::ExtensionLog(_)
                | Delivery::ExtensionJob(..)
                | Delivery::ExtensionUsage(_)
                | Delivery::DelegateUsage { .. }
                | Delivery::Interaction(_)
                | Delivery::Resolved(..)) => {
                    self.admit_idle(now, &mut TurnInput::of(Vec::new()))?;
                }
            }
        }
    }

    /// Applies a reply to `offer`: `true` once `repository_code_resolved`
    /// is written and the reply accepted. A reply that does not fit, or a
    /// decision the seam fails to record, is rejected and the offer stays
    /// pending; non-skip decisions recorded before the failure stay
    /// recorded, while a skip enters `skipped_now` only after the resolved
    /// offer is written, so a rejected reply skips nothing.
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
            // A skip is committed by the resolved offer below, never
            // through the seam: it enters `skipped_now` only after the
            // resolved offer is written, so a rejected reply skips nothing.
            if *decision == OfferDecision::Skip {
                continue;
            }
            let decided = match code.decide(item, *decision) {
                Ok(decided) => decided,
                Err(failure) => {
                    inbox::reject(ack, failure.code, &failure.message);
                    return Ok(false);
                }
            };
            // An approval of an extension arms this process's late step:
            // it runs once before the first preamble, then clears this.
            if *decision == OfferDecision::Approve
                && item.kind == OfferedKind::Extension
                && matches!(decided, Decided::Recorded)
            {
                self.repository.approved_extension = true;
            }
        }
        // A skip lives only in the log and this session, committed only
        // once the resolved offer below is written: collected before the
        // move into the resolved line.
        let skipped: Vec<Key> = offer
            .items
            .iter()
            .zip(&decisions)
            .filter(|(_, decision)| **decision == OfferDecision::Skip)
            .map(|(item, _)| key(item))
            .collect();
        self.log.append(
            &Event::RepositoryCodeResolved(RepositoryCodeResolved {
                request_id: offer.request_id.clone(),
                decisions,
            }),
            None,
            None,
        )?;
        self.repository.decided.extend(offer.items.iter().map(key));
        self.repository.skipped_now.extend(skipped);
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

impl Loop {
    /// Loads what the session's offer step settled, once per process,
    /// before its first preamble: the repository code approved since the
    /// process started. On a loop that already holds a preamble (a rewound
    /// one's logged build) the closure is stored and never called in this
    /// process.
    pub fn approved_code(mut self, load: LoadApproved) -> Self {
        if self.preamble.is_some() {
            self.repository.loaded = true;
        }
        self.repository.load = Some(load);
        self
    }

    /// Prepares the session's extensions again once this process's offer
    /// recorded an approval of an extension, before its first preamble:
    /// the late step writes `extensions_loaded` with the prepared set and
    /// its notices, and no `reloaded`. Unlike the post-offer load, it also
    /// runs on a loop that holds a logged preamble, whose build it drops.
    pub fn late_extensions(mut self, prepare: crate::PrepareReload) -> Self {
        self.repository.late = Some(prepare);
        self
    }

    /// What the offer step settled in this session: the log's folded skips
    /// plus this process's decisions and notices.
    fn outcome(&self) -> OfferOutcome {
        let mut skipped = Vec::new();
        for key in self
            .repository
            .folded
            .skipped
            .iter()
            .chain(self.repository.skipped_now.iter())
        {
            let key = OfferKey::of(key);
            if !skipped.contains(&key) {
                skipped.push(key);
            }
        }
        let mut reported = skipped.clone();
        for key in self
            .repository
            .decided
            .iter()
            .chain(self.repository.noted.iter())
        {
            let key = OfferKey::of(key);
            if !reported.contains(&key) {
                reported.push(key);
            }
        }
        OfferOutcome {
            skipped,
            reported,
            held: Vec::new(),
        }
    }

    /// Runs the post-offer load once per process, immediately before that
    /// process's first build, and never once a preamble existed without a
    /// build in that process. Each event is written through the one
    /// `Transcript` path, and each tool registers as `calls::register`
    /// registers it, except one whose name the load found already held:
    /// what was registered before the load stands, with no replacement
    /// recorded. A later approved tool of a name an earlier approved tool
    /// took still replaces it and is recorded. Then the late step: when
    /// this process's offer recorded an approval of an extension, the
    /// late preparer installs before the first preamble, after the
    /// post-offer load's events. Returns whether the late step installed.
    pub(crate) fn load_approved(&mut self) -> Result<bool, Error> {
        if let Some(load) = self.repository.load.clone()
            && !self.repository.loaded
        {
            // Set before calling, so an `Err` is not retried.
            self.repository.loaded = true;
            let mut outcome = self.outcome();
            // The names held before the load: what was registered stands,
            // with no replacement recorded. Taken before calling, so a
            // collision inside the load still replaces and records, and the
            // load drops the declared rows those names held.
            outcome.held = self.tools.keys().cloned().collect();
            match load(&outcome) {
                Ok(approved) => {
                    for event in &approved.events {
                        self.write(event, None, None)?;
                    }
                    for (by, tool) in approved.tools {
                        let definition = tool.definition();
                        let name = definition.name.clone();
                        if outcome.held.contains(&name) {
                            continue;
                        }
                        if let Some((from, _, _)) = self
                            .tools
                            .insert(name.clone(), (by.clone(), tool, definition))
                        {
                            self.replaced.push(ToolReplaced { name, from, to: by });
                        }
                    }
                }
                Err(failure) => return Err(Error::RepositoryCode(failure)),
            }
        }
        // The late step runs once, on the post-offer load's usual path
        // and on a loop holding a logged preamble alike: a rewound one's
        // server half never runs, but an approval in this process still
        // installs. Cleared before preparing, so a failure is not
        // retried; without a late preparer the flag simply stays armed.
        if !self.repository.approved_extension {
            return Ok(false);
        }
        let Some(prepare) = self.repository.late.clone() else {
            return Ok(false);
        };
        self.repository.approved_extension = false;
        let route = crate::ReloadRoute {
            model: self.model.reference.clone(),
            credential: self.prompt.credential.clone(),
        };
        match prepare(&route) {
            Ok(prepared) => {
                self.apply_late(prepared)?;
                Ok(true)
            }
            Err(rejection) => Err(Error::RepositoryCode(Failure {
                code: rejection.code,
                message: rejection.message,
                retry_after_ms: None,
                provider: None,
            })),
        }
    }
}

impl crate::resume::Resumed {
    /// Every content the session skipped, as the post-offer load takes it.
    pub fn skipped(&self) -> Vec<OfferKey> {
        self.offers.skipped.iter().map(OfferKey::of).collect()
    }
}
