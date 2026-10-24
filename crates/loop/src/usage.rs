//! The session's usage ledger (`docs/events.md`, `usage` and
//! `usage_recorded`). One implementation folds the process `Summary` and
//! the spending budget, so the two cannot drift.

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError};

use contract::events::{Event, Notice, UsageRecorded};
use contract::provider::{CallUsage, Cost, CostLookup, Tier};
use contract::shapes::{Tokens, Usage};
use contract::{ActionId, TurnId};
use contract::{ErrorCode, GenerationId};

/// The call's cost in US dollars (`docs/model-routing.md`, "Cost").
///
/// The prompt is `input` plus `cache_read` plus every `cache_write`, each
/// sum saturating so a malformed count cannot abort the session. The tier
/// with the highest `input_tokens_above` strictly below that count supplies
/// every price; a count equal to a threshold stays on the tier below it.
/// Two tiers with the same threshold: the first listed wins. Below every
/// threshold, the base prices apply, and a missing cache price is 0.
/// A `"1h"` write costs twice the selected input price; any other key
/// costs the selected `cache_write` price.
pub(crate) fn price(cost: &Cost, tokens: &Tokens) -> f64 {
    let hour = tokens.cache_write.get("1h").copied().unwrap_or(0);
    let five = tokens
        .cache_write
        .iter()
        .filter(|(key, _)| key.as_str() != "1h")
        .map(|(_, &count)| count)
        .fold(0, u64::saturating_add);
    let written = five.saturating_add(hour);
    let prompt = tokens
        .input
        .saturating_add(tokens.cache_read)
        .saturating_add(written);
    let (input, output, cache_read, cache_write) = match tier(cost, prompt) {
        Some(tier) => (tier.input, tier.output, tier.cache_read, tier.cache_write),
        None => (
            cost.input,
            cost.output,
            cost.cache_read.unwrap_or(0.0),
            cost.cache_write.unwrap_or(0.0),
        ),
    };
    (input * tokens.input as f64
        + cache_read * tokens.cache_read as f64
        + cache_write * five as f64
        + 2.0 * input * hour as f64
        + output * tokens.output as f64)
        / 1_000_000.0
}

/// The tier whose threshold is the highest one `prompt` exceeds. Reversing
/// first makes an equal threshold keep the tier listed earlier: `max_by_key`
/// returns the last maximum.
fn tier(cost: &Cost, prompt: u64) -> Option<&Tier> {
    cost.tiers
        .iter()
        .rev()
        .filter(|tier| tier.input_tokens_above < prompt)
        .max_by_key(|tier| tier.input_tokens_above)
}

/// The cost one call is recorded at (`docs/model-routing.md`, "Cost"): the
/// vendor's own figure where the reply reports one, otherwise the model's
/// declared prices applied to the call's tokens, else `None`.
pub(crate) fn call_cost(
    inline: Option<f64>,
    prices: Option<&Cost>,
    tokens: &Tokens,
) -> Option<f64> {
    inline.or_else(|| prices.map(|prices| price(prices, tokens)))
}

/// A call's `usage_recorded`, and whether its cost may be looked up later.
pub(crate) struct Recorded {
    /// The line.
    pub(crate) line: UsageRecorded,
    /// True for a call without the vendor's own figure whose id the provider
    /// named: only such a call is looked up (`docs/model-routing.md`,
    /// "Cost").
    pub(crate) lookable: bool,
}

/// One `usage_recorded` from what a call reported (`docs/events.md`,
/// `usage_recorded`). A partial record carries no vendor figure, so its
/// `cost` is the declared prices applied to its tokens. A call the provider
/// named no generation for, or an empty one, gets an id Fiber mints,
/// starting `fiber-`, once, here: its copies and its fold all carry that id.
/// Such a call's `cost` is `null` and is never looked up, because the vendor
/// never named the generation (`docs/model-routing.md`, "Cost").
pub(crate) fn recorded(
    usage: CallUsage,
    inline_cost: Option<f64>,
    model: &str,
    prices: Option<&Cost>,
    subscription: bool,
) -> Recorded {
    let named = usage.generation_id.filter(|id| !id.0.is_empty());
    let cost = match named {
        Some(_) => call_cost(inline_cost, prices, &usage.tokens),
        None => None,
    };
    let lookable = named.is_some() && inline_cost.is_none();
    let line = UsageRecorded {
        generation_id: named.unwrap_or_else(|| GenerationId(crate::mint("fiber-"))),
        model: model.to_owned(),
        tokens: usage.tokens,
        input_bytes: usage.input_size.bytes,
        input_media: usage.input_size.media.then_some(true),
        web_searches: usage.web_searches,
        cost,
        subscription: subscription.then_some(true),
        extension: None,
        origin_session_id: None,
        reviewer: None,
    };
    Recorded { line, lookable }
}

/// One `usage_recorded` for an extension's `host.model` call: `recorded`
/// with `extension` set, so the loop ledger counts it under the calling
/// extension (`docs/extensions.md`, "Host calls").
pub fn extension_line(
    usage: CallUsage,
    inline_cost: Option<f64>,
    model: &str,
    prices: Option<&Cost>,
    subscription: bool,
    extension: &str,
) -> UsageRecorded {
    let mut recorded = recorded(usage, inline_cost, model, prices, subscription);
    recorded.line.extension = Some(extension.to_owned());
    recorded.line
}

impl crate::Loop {
    /// Writes and ledgers a call's usage that ended without a reply.
    pub(crate) fn write_unfinished(
        &mut self,
        usage: &CallUsage,
        turn: &TurnId,
        message: &ActionId,
    ) -> Result<(), crate::Error> {
        let model = self.model.reference.clone();
        let prices = self.model.cost.clone();
        let subscription = self.model.subscription;
        let recorded = recorded(usage.clone(), None, &model, prices.as_ref(), subscription);
        let lookup = self.provider.cost_lookup();
        self.write_usage(recorded, lookup, Some(turn), Some(message))
    }

    /// Writes and ledgers one call's `usage_recorded`: the one path every
    /// call's usage takes. A `lookable` record whose provider has a lookup
    /// is looked up once, later (`docs/model-routing.md`, "Cost").
    pub(crate) fn write_usage(
        &mut self,
        recorded: Recorded,
        lookup: Option<Arc<dyn CostLookup>>,
        turn: Option<&TurnId>,
        action: Option<&ActionId>,
    ) -> Result<(), crate::Error> {
        let Recorded { line, lookable } = recorded;
        self.put_usage(&line, turn, action)?;
        self.record_usage(&line);
        let Some(lookup) = lookup.filter(|_| lookable) else {
            return Ok(());
        };
        let clock = Arc::clone(self.log.clock());
        let scheduled =
            self.late_cost
                .schedule(lookup, line, turn.cloned(), action.cloned(), &clock);
        if let Err(e) = scheduled {
            self.log.append(
                &Event::Notice(Notice {
                    code: ErrorCode::IoFailed,
                    message: format!(
                        "The cost lookup could not start: {e}. The call's cost stays as first recorded."
                    ),
                    extension: None,
                }),
                turn.cloned(),
                None,
            )?;
        }
        Ok(())
    }

    /// Writes every record whose cost settled late, each in its first
    /// record's turn and action (`docs/events.md`, "Usage and notices").
    pub(crate) fn write_settled(&mut self) -> Result<(), crate::Error> {
        for settled in self.late_cost.take_settled() {
            self.put_usage(
                &settled.record,
                settled.turn.as_ref(),
                settled.action.as_ref(),
            )?;
            self.record_usage(&settled.record);
        }
        Ok(())
    }

    /// At the session's end: stops the lookups, so nothing settles after
    /// this, then writes what already settled. A lookup still waiting is
    /// never made.
    pub(crate) fn write_last_settled(&mut self) -> Result<(), crate::Error> {
        self.late_cost.stop();
        self.write_settled()
    }

    fn put_usage(
        &mut self,
        recorded: &UsageRecorded,
        turn: Option<&TurnId>,
        action: Option<&ActionId>,
    ) -> Result<(), crate::Error> {
        // Writes and ledgers only: the tripwire update runs after this
        // returns, so a failed append never touches the trip and a copy
        // never folds its older line over a newer watcher value. Own
        // calls follow with `record_usage`; copies follow with
        // `publish_usage` on the acknowledgment path.
        self.write(&Event::UsageRecorded(recorded.clone()), turn, action)?;
        self.ledger.record(recorded);
        Ok(())
    }

    /// Names the originator on a delegate's `usage_recorded` copy: the
    /// sending delegate when absent, preserved when present so a
    /// grandchild's line still names the originator. The one
    /// implementation live copying and backfill share, so the two cannot
    /// drift.
    pub(crate) fn normalize_copy(
        session: &contract::SessionId,
        line: UsageRecorded,
    ) -> UsageRecorded {
        let mut copied = line;
        if copied.origin_session_id.is_none() {
            copied.origin_session_id = Some(session.clone());
        }
        copied
    }

    /// Copies a delegate's `usage_recorded` into this log: sets
    /// `origin_session_id` to the sending delegate when absent, preserves
    /// it when present so a grandchild's line still names the originator,
    /// then writes and ledgers it with no turn and no action. Copies are
    /// never looked up, so they never schedule a late cost. A copy that
    /// claims the one-way trip stops delegates at once, as a tap arrival
    /// does: foreign spend arriving mid-turn or mid-wait is stopped
    /// promptly, while the loop's own spend waits for the request-time
    /// budget end.
    pub(crate) fn copy_usage(
        &mut self,
        session: &contract::SessionId,
        line: UsageRecorded,
    ) -> Result<(), crate::Error> {
        let copied = Self::normalize_copy(session, line);
        self.put_usage(&copied, None, None)?;
        self.publish_usage(&copied);
        Ok(())
    }

    /// Records one ledgered line on the spend tripwire without claiming
    /// the trip: the loop's own spend waits for the request-time budget
    /// end, so an over-budget turn stops delegates exactly once there.
    /// Only copies and tap arrivals claim, for a prompt stop while the
    /// loop is parked or mid-turn on foreign spend.
    fn record_usage(&self, recorded: &UsageRecorded) {
        self.spend_trip
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(recorded);
    }

    /// Publishes one ledgered line to the spend tripwire, stopping
    /// delegates at once when it claims the one-way trip. The acknowledgment
    /// consumes one tapped arrival not yet folded and keeps the watcher's
    /// latest value; with no arrival pending (a direct or backfilled copy)
    /// it folds the copy's own delta. The tripwire lock is released before
    /// the stop, and the stop tolerates ended jobs, so a trip racing a job
    /// end stops nothing twice.
    fn publish_usage(&self, recorded: &UsageRecorded) {
        let tripped = {
            let mut trip = self
                .spend_trip
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            trip.ack(recorded)
        };
        if tripped && let Some(jobs) = &self.ending.jobs {
            jobs.stop_delegates();
        }
    }
}

/// The latest `usage_recorded` per `generation_id`, in first-recorded
/// order, with the fold's running state beside it. A later line with an id
/// already recorded replaces the earlier one in every total, so a copy of
/// a copy is still one call (`docs/events.md`, `usage_recorded`). After
/// any sequence of records the usage equals [`log::usage`] over the held
/// lines exactly: a new call takes one step, and a correction folds the
/// whole ledger again.
#[derive(Debug, Clone)]
pub(crate) struct Ledger {
    calls: Vec<UsageRecorded>,
    index: BTreeMap<GenerationId, usize>,
    tokens: Tokens,
    billed: bool,
    cost: Option<f64>,
    subscription_cost: f64,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            index: BTreeMap::new(),
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 0,
            },
            billed: false,
            cost: None,
            subscription_cost: 0.0,
        }
    }
}

impl Ledger {
    /// Keeps `line`, replacing any earlier line with its `generation_id`
    /// in place, keeping first-recorded order. True when it replaced one:
    /// `line` corrects a call already counted.
    pub(crate) fn record(&mut self, line: &UsageRecorded) -> bool {
        match self.index.get(&line.generation_id).copied() {
            Some(at) => {
                // An identical correction changes nothing; any other one
                // folds the whole ledger again, so only corrections pay
                // O(n). The index only holds positions of held lines.
                if self.calls.get(at).is_some_and(|held| held != line)
                    && let Some(held) = self.calls.get_mut(at)
                {
                    *held = line.clone();
                    self.recount();
                }
                true
            }
            None => {
                self.index
                    .insert(line.generation_id.clone(), self.calls.len());
                self.calls.push(line.clone());
                self.add(line);
                false
            }
        }
    }

    /// One step of the fold [`log::usage`] runs: what a new call adds.
    fn add(&mut self, call: &UsageRecorded) {
        self.tokens.input = self.tokens.input.saturating_add(call.tokens.input);
        self.tokens.cache_read = self
            .tokens
            .cache_read
            .saturating_add(call.tokens.cache_read);
        self.tokens.output = self.tokens.output.saturating_add(call.tokens.output);
        for (lifetime, count) in &call.tokens.cache_write {
            let total = self.tokens.cache_write.entry(lifetime.clone()).or_default();
            *total = total.saturating_add(*count);
        }
        if call.subscription == Some(true) {
            self.subscription_cost += call.cost.unwrap_or(0.0);
        } else {
            self.billed = true;
            if let Some(known) = call.cost {
                *self.cost.get_or_insert(0.0) += known;
            }
        }
    }

    /// Folds the held lines again from scratch.
    fn recount(&mut self) {
        let calls = std::mem::take(&mut self.calls);
        self.tokens = Tokens {
            input: 0,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 0,
        };
        self.billed = false;
        self.cost = None;
        self.subscription_cost = 0.0;
        for call in &calls {
            self.add(call);
        }
        self.calls = calls;
    }

    /// The docs' `usage` shape over the lines kept.
    pub(crate) fn usage(&self) -> Usage {
        Usage {
            tokens: self.tokens.clone(),
            // `docs/events.md`, `usage`: 0 with no billed call, null when
            // billed calls exist and none had a known cost.
            cost: if self.billed { self.cost } else { Some(0.0) },
            subscription_cost: self.subscription_cost,
        }
    }

    /// US dollars billed per token. A `cost` still null counts as zero
    /// (`docs/loop.md`, "Spending budget"). Subscription calls are absent
    /// from the sum.
    pub(crate) fn spend(&self) -> f64 {
        self.usage().cost.unwrap_or(0.0)
    }

    /// The held lines, in first-recorded order: the tripwire starts from
    /// these, so a resume keeps its restored spend.
    pub(crate) fn lines(&self) -> &[UsageRecorded] {
        &self.calls
    }
}

/// The spend the loop has seen, shared with the delegate-socket reader:
/// one leaf mutex protecting the per-generation billed-cost map, the
/// accumulated billed spend, the optional budget, the one-way tripped
/// flag and the count of tapped arrivals not yet folded per generation.
/// Every access holds it; the stop and the inbox send run after it
/// is released, so the watcher never holds it across a stop or a send.
/// The pending count is process-local coordination, never durable state:
/// a resume rebuilds the map from the ledger with no arrivals pending.
#[derive(Debug)]
pub(crate) struct Tripwire {
    billed: BTreeMap<GenerationId, f64>,
    sum: f64,
    budget: Option<f64>,
    tripped: bool,
    pending: BTreeMap<GenerationId, usize>,
}

impl Tripwire {
    /// The billed dollars one line counts: zero for subscription lines
    /// and for lines with no known cost.
    pub(crate) fn billed_cost(line: &UsageRecorded) -> f64 {
        if line.subscription == Some(true) {
            0.0
        } else {
            line.cost.unwrap_or(0.0)
        }
    }

    /// Starts from the ledger's held lines and the session budget.
    pub(crate) fn from_ledger(ledger: &Ledger, budget: Option<f64>) -> Self {
        let mut trip = Self {
            billed: BTreeMap::new(),
            sum: 0.0,
            budget,
            tripped: false,
            pending: BTreeMap::new(),
        };
        for line in ledger.lines() {
            let cost = Self::billed_cost(line);
            trip.billed.insert(line.generation_id.clone(), cost);
            trip.sum += cost;
        }
        trip
    }

    /// Folds one line's delta (`billed(new) - billed(old)`), so a
    /// correction adds only its difference and a re-delivery adds
    /// nothing.
    fn fold(&mut self, line: &UsageRecorded) {
        let new = Self::billed_cost(line);
        let old = self.billed.get(&line.generation_id).copied().unwrap_or(0.0);
        let known = self.billed.contains_key(&line.generation_id);
        if !known || new != old {
            self.billed.insert(line.generation_id.clone(), new);
            self.sum += new - old;
        }
    }

    /// Records one ledgered line on the spend tripwire without claiming
    /// the trip: loop-side publication only reconciles the sum, leaving
    /// the claim to tap arrivals, copies and the request-time budget end.
    pub(crate) fn record(&mut self, line: &UsageRecorded) {
        self.fold(line);
    }

    /// Applies one line's delta, true when this call claims the one-way
    /// trip: billed spend reached the budget. A downward correction never
    /// untrips.
    pub(crate) fn apply(&mut self, line: &UsageRecorded) -> bool {
        self.fold(line);
        self.claim()
    }

    /// Taps one watcher arrival: counts it as pending and folds its delta
    /// atomically, true when this call claims the one-way trip. The watcher
    /// runs this before offering the arrival's delivery, under the same
    /// lock, so a loop fold racing the offer never replays an older line
    /// over this newer value.
    pub(crate) fn tap(&mut self, line: &UsageRecorded) -> bool {
        *self.pending.entry(line.generation_id.clone()).or_insert(0) += 1;
        self.fold(line);
        self.claim()
    }

    /// Acknowledges one loop-folded copy: with a tapped arrival of its
    /// generation still pending, consumes one without folding the copy's
    /// (older) line again, preserving the watcher's latest value through
    /// all older acknowledgments, including downward corrections and
    /// zero-cost records. With nothing pending (a direct or backfilled
    /// copy) folds the copy's own delta. Either way checks the one-way
    /// trip on the current sum under the same lock hold.
    pub(crate) fn ack(&mut self, line: &UsageRecorded) -> bool {
        // A present entry always counts a still-pending arrival: taps
        // insert one and this acknowledgment removes the entry at zero,
        // and nothing else writes the map, so there is no zero-count
        // entry to guard against. A `count > 0` check here would leave
        // a mutant (`>=`) no test can catch, since the comparison is
        // always true on a present entry.
        if let Some(count) = self.pending.get_mut(&line.generation_id) {
            *count -= 1;
            if *count == 0 {
                self.pending.remove(&line.generation_id);
            }
            return self.claim();
        }
        self.apply(line)
    }

    /// Whether the one-way trip has been claimed: the tree is over
    /// budget, so its stop is issued or imminent for this session.
    pub(crate) fn tripped(&self) -> bool {
        self.tripped
    }

    /// Checks the one-way trip on the current sum: true once, when the
    /// sum first reaches the budget. A downward correction never untrips.
    fn claim(&mut self) -> bool {
        if self.tripped {
            return false;
        }
        if let Some(limit) = self.budget
            && self.sum >= limit
        {
            self.tripped = true;
            return true;
        }
        false
    }

    /// Publishes one restored ledger line the map lacks, preserving a
    /// watcher contribution not yet folded: a ledger snapshot older than a
    /// racing watcher update never overwrites it. Recording only, like
    /// every loop-side publication.
    pub(crate) fn publish_missing(&mut self, line: &UsageRecorded) {
        if self.billed.contains_key(&line.generation_id) {
            return;
        }
        let cost = Self::billed_cost(line);
        self.billed.insert(line.generation_id.clone(), cost);
        self.sum += cost;
    }

    /// Publishes the session budget. Recording only: the trip is
    /// claimed by spend arrivals and by the request-time budget end,
    /// so attaching a budget never stops delegates on its own.
    pub(crate) fn set_budget(&mut self, budget: Option<f64>) {
        self.budget = budget;
    }
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;

/// The pending-arrival acknowledgment against an older copy replaying a
/// newer tap (`Sol settlement 2`): the watcher counts each tapped arrival
/// and folds it atomically; the loop's copy fold consumes one pending
/// arrival without folding the older line again. Each test forces its
/// order with channels, never sleeps, and fails when the acknowledgment
/// is removed (the older line replays, the sum is wrong and the trip is
/// missed).
#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code signals ordering through channels"
)]
mod pending_tests {
    use contract::GenerationId;
    use contract::events::UsageRecorded;
    use contract::shapes::Tokens;
    use fakes::Deadline;
    use fakes::MUST_SUCCEED_WITHIN;
    use std::collections::BTreeMap;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    use super::{Ledger, Tripwire};

    /// A billed line for `id` at `cost`: what the tripwire folds is only
    /// the generation, the cost and the subscription flag.
    fn cost_line(id: &str, cost: Option<f64>) -> UsageRecorded {
        UsageRecorded {
            generation_id: GenerationId(id.into()),
            model: "fake/m".into(),
            tokens: Tokens {
                input: 10,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 3,
            },
            input_bytes: 0,
            input_media: None,
            web_searches: None,
            cost,
            subscription: None,
            extension: None,
            origin_session_id: None,
            reviewer: None,
        }
    }

    /// The tripwire sum under its lock.
    fn sum_of(trip: &Arc<Mutex<Tripwire>>) -> f64 {
        trip.lock().unwrap().sum
    }

    /// Taps `line` on `trip`, counting a claimed trip in `stops`.
    fn tap(trip: &Arc<Mutex<Tripwire>>, stops: &Arc<AtomicUsize>, line: &UsageRecorded) -> bool {
        let tripped = trip.lock().unwrap().tap(line);
        if tripped {
            stops.fetch_add(1, Ordering::SeqCst);
        }
        tripped
    }

    /// Acknowledges `line` on `trip` while recording it in `ledger`,
    /// counting a claimed trip in `stops`.
    fn ack(
        trip: &Arc<Mutex<Tripwire>>,
        ledger: &Arc<Mutex<Ledger>>,
        stops: &Arc<AtomicUsize>,
        line: &UsageRecorded,
    ) -> bool {
        ledger.lock().unwrap().record(line);
        let tripped = trip.lock().unwrap().ack(line);
        if tripped {
            stops.fetch_add(1, Ordering::SeqCst);
        }
        tripped
    }

    #[test]
    fn an_older_copy_ack_keeps_the_newer_tap_and_a_sibling_trips() {
        // Budget $4: watcher taps g at $1 then its $3 correction while
        // both deliveries are held; the loop folds only g's $1 delivery;
        // watcher h at $1 trips before the $3 correction folds. Without
        // the acknowledgment the $1 fold replays over $3, the map totals
        // $2 at h and the stop is missed.
        let trip = Arc::new(Mutex::new(Tripwire::from_ledger(
            &Ledger::default(),
            Some(4.0),
        )));
        let ledger = Arc::new(Mutex::new(Ledger::default()));
        let stops = Arc::new(AtomicUsize::new(0));
        let (both_tapped_tx, both_tapped_rx) = mpsc::channel::<()>();
        let (folded_g1_tx, folded_g1_rx) = mpsc::channel::<()>();
        let (tapped_h_tx, tapped_h_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        std::thread::scope(|scope| {
            scope.spawn({
                let trip = Arc::clone(&trip);
                let stops = Arc::clone(&stops);
                move || {
                    assert!(!tap(&trip, &stops, &cost_line("g", Some(1.0))));
                    assert!(!tap(&trip, &stops, &cost_line("g", Some(3.0))));
                    both_tapped_tx.send(()).unwrap();
                    Deadline::after(MUST_SUCCEED_WITHIN)
                        .recv(&folded_g1_rx)
                        .expect("the loop folds g's $1 delivery");
                    // The sibling arrives before the correction folds: $3 + $1
                    // reaches $4, so this tap claims the one-way trip.
                    assert!(tap(&trip, &stops, &cost_line("h", Some(1.0))));
                    assert_eq!(stops.load(Ordering::SeqCst), 1);
                    assert!((sum_of(&trip) - 4.0).abs() < 1e-9);
                    tapped_h_tx.send(()).unwrap();
                }
            });
            scope.spawn({
                let trip = Arc::clone(&trip);
                let ledger = Arc::clone(&ledger);
                let stops = Arc::clone(&stops);
                move || {
                    Deadline::after(MUST_SUCCEED_WITHIN)
                        .recv(&both_tapped_rx)
                        .expect("the watcher taps g twice");
                    // Folds only the older $1 delivery: the acknowledgment keeps
                    // the watcher's $3, so the sum stays $3.
                    assert!(!ack(&trip, &ledger, &stops, &cost_line("g", Some(1.0))));
                    assert!((sum_of(&trip) - 3.0).abs() < 1e-9);
                    assert_eq!(stops.load(Ordering::SeqCst), 0);
                    folded_g1_tx.send(()).unwrap();
                    Deadline::after(MUST_SUCCEED_WITHIN)
                        .recv(&tapped_h_rx)
                        .expect("the watcher taps h");
                    // The watcher already stopped delegates exactly once at h;
                    // the held correction and the sibling fold without
                    // claiming again, and the ledger totals $4.
                    assert_eq!(stops.load(Ordering::SeqCst), 1);
                    assert!(!ack(&trip, &ledger, &stops, &cost_line("g", Some(3.0))));
                    assert!(!ack(&trip, &ledger, &stops, &cost_line("h", Some(1.0))));
                    assert!((ledger.lock().unwrap().spend() - 4.0).abs() < 1e-9);
                    assert!((sum_of(&trip) - 4.0).abs() < 1e-9);
                    assert_eq!(stops.load(Ordering::SeqCst), 1);
                    done_tx.send(()).unwrap();
                }
            });
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&done_rx)
                .expect("the loop folds the correction and h");
        });
        assert!((ledger.lock().unwrap().spend() - 4.0).abs() < 1e-9);
        assert!((sum_of(&trip) - 4.0).abs() < 1e-9);
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_newer_copy_ack_first_still_keeps_the_newer_tap() {
        // The reverse ordering: both g deliveries held, the loop folds
        // the newer $3 first, then the older $1. Both acknowledgments keep
        // $3; the sibling still trips at $4. Without the acknowledgment
        // the older $1 replays over $3 and the sum is wrong.
        let trip = Arc::new(Mutex::new(Tripwire::from_ledger(
            &Ledger::default(),
            Some(4.0),
        )));
        let ledger = Arc::new(Mutex::new(Ledger::default()));
        let stops = Arc::new(AtomicUsize::new(0));
        let (both_tapped_tx, both_tapped_rx) = mpsc::channel::<()>();
        let (folded_new_tx, folded_new_rx) = mpsc::channel::<()>();
        let (tapped_h_tx, tapped_h_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        std::thread::scope(|scope| {
            scope.spawn({
                let trip = Arc::clone(&trip);
                let stops = Arc::clone(&stops);
                move || {
                    assert!(!tap(&trip, &stops, &cost_line("g", Some(1.0))));
                    assert!(!tap(&trip, &stops, &cost_line("g", Some(3.0))));
                    both_tapped_tx.send(()).unwrap();
                    Deadline::after(MUST_SUCCEED_WITHIN)
                        .recv(&folded_new_rx)
                        .expect("the loop folds g's $3 delivery");
                    assert!(tap(&trip, &stops, &cost_line("h", Some(1.0))));
                    assert_eq!(stops.load(Ordering::SeqCst), 1);
                    tapped_h_tx.send(()).unwrap();
                }
            });
            scope.spawn({
                let trip = Arc::clone(&trip);
                let ledger = Arc::clone(&ledger);
                let stops = Arc::clone(&stops);
                move || {
                    Deadline::after(MUST_SUCCEED_WITHIN)
                        .recv(&both_tapped_rx)
                        .expect("the watcher taps g twice");
                    assert!(!ack(&trip, &ledger, &stops, &cost_line("g", Some(3.0))));
                    assert!((sum_of(&trip) - 3.0).abs() < 1e-9);
                    folded_new_tx.send(()).unwrap();
                    Deadline::after(MUST_SUCCEED_WITHIN)
                        .recv(&tapped_h_rx)
                        .expect("the watcher taps h");
                    // The older $1 acknowledgment keeps the watcher's $3, so the
                    // tripwire still totals $4 at h. The ledger follows fold
                    // order, so this artificial reverse fold leaves it holding
                    // the older $1 (spend $2); production folds in delivery
                    // order, as the first test proves, and the ledger totals
                    // $4 there.
                    assert!(!ack(&trip, &ledger, &stops, &cost_line("g", Some(1.0))));
                    assert!((sum_of(&trip) - 4.0).abs() < 1e-9);
                    assert!(!ack(&trip, &ledger, &stops, &cost_line("h", Some(1.0))));
                    assert!((ledger.lock().unwrap().spend() - 2.0).abs() < 1e-9);
                    assert!((sum_of(&trip) - 4.0).abs() < 1e-9);
                    assert_eq!(stops.load(Ordering::SeqCst), 1);
                    done_tx.send(()).unwrap();
                }
            });
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&done_rx)
                .expect("the loop folds the older correction");
        });
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_downward_correction_ack_keeps_the_lower_tap() {
        // Budget $2: watcher taps g at $3 then its downward correction to
        // $1 while both deliveries are held; the loop folds the older $3
        // first without replaying over $1, so no second claim; h at $1 then
        // holds $2 with the trip already claimed once. The ledger totals $2.
        let trip = Arc::new(Mutex::new(Tripwire::from_ledger(
            &Ledger::default(),
            Some(2.0),
        )));
        let ledger = Arc::new(Mutex::new(Ledger::default()));
        let stops = Arc::new(AtomicUsize::new(0));
        let (both_tapped_tx, both_tapped_rx) = mpsc::channel::<()>();
        let (folded_old_tx, folded_old_rx) = mpsc::channel::<()>();
        let (tapped_h_tx, tapped_h_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        std::thread::scope(|scope| {
            scope.spawn({
                let trip = Arc::clone(&trip);
                let stops = Arc::clone(&stops);
                move || {
                    assert!(tap(&trip, &stops, &cost_line("g", Some(3.0))));
                    assert_eq!(stops.load(Ordering::SeqCst), 1);
                    assert!(!tap(&trip, &stops, &cost_line("g", Some(1.0))));
                    // A downward correction never untrips: the first tap
                    // already claimed, the second claims nothing more.
                    assert_eq!(stops.load(Ordering::SeqCst), 1);
                    both_tapped_tx.send(()).unwrap();
                    Deadline::after(MUST_SUCCEED_WITHIN)
                        .recv(&folded_old_rx)
                        .expect("the loop folds g's $3 delivery");
                    assert!(!tap(&trip, &stops, &cost_line("h", Some(1.0))));
                    assert_eq!(stops.load(Ordering::SeqCst), 1);
                    tapped_h_tx.send(()).unwrap();
                }
            });
            scope.spawn({
                let trip = Arc::clone(&trip);
                let ledger = Arc::clone(&ledger);
                let stops = Arc::clone(&stops);
                move || {
                    Deadline::after(MUST_SUCCEED_WITHIN)
                        .recv(&both_tapped_rx)
                        .expect("the watcher taps g twice");
                    // The older $3 acknowledgment keeps the watcher's $1.
                    assert!(!ack(&trip, &ledger, &stops, &cost_line("g", Some(3.0))));
                    assert!((sum_of(&trip) - 1.0).abs() < 1e-9);
                    folded_old_tx.send(()).unwrap();
                    Deadline::after(MUST_SUCCEED_WITHIN)
                        .recv(&tapped_h_rx)
                        .expect("the watcher taps h");
                    assert!(!ack(&trip, &ledger, &stops, &cost_line("g", Some(1.0))));
                    assert!((sum_of(&trip) - 2.0).abs() < 1e-9);
                    assert!(!ack(&trip, &ledger, &stops, &cost_line("h", Some(1.0))));
                    assert!((ledger.lock().unwrap().spend() - 2.0).abs() < 1e-9);
                    assert_eq!(stops.load(Ordering::SeqCst), 1);
                    done_tx.send(()).unwrap();
                }
            });
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv(&done_rx)
                .expect("the loop folds the correction");
        });
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_copy_with_nothing_pending_folds_its_own_delta() {
        // A directly delivered or backfilled copy with no tapped arrival
        // pending applies its own delta, so existing direct-copy,
        // null-cost and subscription behaviour holds.
        let mut trip = Tripwire::from_ledger(&Ledger::default(), Some(4.0));
        assert!(!trip.ack(&cost_line("g", Some(1.0))));
        assert!((trip.sum - 1.0).abs() < 1e-9);
        assert!(!trip.ack(&cost_line("n", None)));
        assert!((trip.sum - 1.0).abs() < 1e-9);
        let mut subscription = cost_line("s", Some(2.0));
        subscription.subscription = Some(true);
        assert!(!trip.ack(&subscription));
        assert!((trip.sum - 1.0).abs() < 1e-9);
        assert!(trip.ack(&cost_line("h", Some(3.0))));
        assert!((trip.sum - 4.0).abs() < 1e-9);
    }

    #[test]
    fn a_second_ack_with_nothing_pending_folds_its_own_delta() {
        // g tapped once at $4, then acknowledged twice with the older
        // $1 line: the first acknowledgment consumes the pending
        // arrival without folding, keeping $4; the second finds
        // nothing pending and folds its own line as a correction,
        // totaling $1. A consume that never removes the entry (`/=`)
        // keeps consuming and never folds; one that grows the count
        // (`+=`) does the same; both leave the sum at $4.
        let mut trip = Tripwire::from_ledger(&Ledger::default(), None);
        assert!(!trip.tap(&cost_line("g", Some(4.0))));
        assert!((trip.sum - 4.0).abs() < 1e-9);
        assert!(!trip.ack(&cost_line("g", Some(1.0))));
        assert!((trip.sum - 4.0).abs() < 1e-9);
        assert!(!trip.ack(&cost_line("g", Some(1.0))));
        assert!((trip.sum - 1.0).abs() < 1e-9);
    }
}
