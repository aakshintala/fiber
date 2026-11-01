//! Chrome slots' renderer inputs: the multi-row slot structs, with the
//! cache keys over the slot and every field. Chrome inputs hold source
//! data only; the frame wall time travels beside `frame` on the
//! invocation path, never in these structs.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use contract::ErrorCode;
use contract::events::{Outcome, QuotaInfo};
use contract::shapes::Usage;

use super::slots::{PromptImage, hash_images};

/// The working line: how long the turn runs, or its wait to retry.
#[derive(Debug, Clone)]
pub struct WorkingLine {
    /// When the turn started, as the turn card's value; none before its
    /// `turn_started`.
    pub started: Option<u64>,
    /// The open turn's wait to retry, while one pends.
    pub retry: Option<WorkingRetry>,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// The working line's wait to retry: the failed call's code, the attempt
/// about to be made, the last attempt the policy allows, the wait before
/// it, and when it was scheduled.
#[derive(Debug, Clone)]
pub struct WorkingRetry {
    /// The failed call's `error.code`.
    pub code: ErrorCode,
    /// The attempt about to be made.
    pub attempt: u32,
    /// The last attempt the retry policy allows.
    pub last_attempt: u32,
    /// The wait before it, in milliseconds.
    pub delay_ms: u64,
    /// The `retry_scheduled` envelope's `ts`, in milliseconds.
    pub ts: u64,
}

/// One queued steering message's image, as the queue fold retains it:
/// the file's path and its size in pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedImage {
    /// The file's path in the session's `artifacts/`.
    pub path: String,
    /// The image's width in pixels.
    pub width: u32,
    /// The image's height in pixels.
    pub height: u32,
}

/// One queued steering message: its text and images with what the
/// built-in draws from them.
#[derive(Debug, Clone)]
pub struct SteeringMessage<'a> {
    /// The message's text.
    pub text: &'a str,
    /// Its images, in order.
    pub images: Vec<PromptImage<'a>>,
    /// Whether a `steer` sent it: such a row draws its `✕`.
    pub droppable: bool,
    /// Whether the person can pick it: droppable with no image.
    pub selectable: bool,
    /// Whether it holds the person's up/down choice.
    pub selected: bool,
}

/// The queued steering messages, oldest first.
#[derive(Debug, Clone)]
pub struct SteeringQueue<'a> {
    /// The queued messages, oldest first.
    pub messages: Vec<SteeringMessage<'a>>,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// The Session card's spend: the status spend whole, or the attached
/// spend's cost alone when the session reports none.
#[derive(Debug, Clone)]
pub enum Spend<'a> {
    /// The session status spend: its cost with the token totals.
    Status(&'a Usage),
    /// The attached spend fallback: its cost alone.
    Attached {
        /// The attached session's spend so far, in US dollars.
        cost: f64,
    },
}

/// The quota answer: the provider credential labels' reported quota in
/// wire order, with the session's threshold.
#[derive(Debug, Clone)]
pub struct Quota<'a> {
    /// The quota answer's rows, ordered by provider name, then
    /// credential label.
    pub rows: &'a [QuotaInfo],
    /// The session's percent-used threshold.
    pub notice_at: f64,
}

/// The branch the Session card shows: the last query's answer, else the
/// status's git.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchName<'a> {
    /// On a branch.
    Named(&'a str),
    /// `HEAD`: detached, or the status's missing branch.
    Detached,
}

/// The Session card's context: the latest request's size against the
/// preamble's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionContext {
    /// The context's size in tokens.
    pub tokens: u64,
    /// The model's context window, in tokens.
    pub window: u64,
    /// The latest preamble's handoff point, in tokens.
    pub trigger_at: Option<u64>,
}

/// The Session card: the attached session as the card reads it.
#[derive(Debug, Clone)]
pub struct SessionCard<'a> {
    /// The workspace's path; none before the first fold.
    pub workspace: Option<&'a str>,
    /// The branch from `git status`; none when unknown.
    pub branch: Option<BranchName<'a>>,
    /// The model the latest preamble names.
    pub model: Option<&'a str>,
    /// The thinking level the latest preamble names.
    pub thinking: Option<&'a str>,
    /// Whether the model is thinking now.
    pub thinking_now: bool,
    /// The latest request's context; none until a window and a fill.
    pub context: Option<SessionContext>,
    /// When the prompt cache expires, as `ts`; none before the first
    /// request.
    pub cache_expires: Option<u64>,
    /// The spend: the status spend whole, or the attached fallback.
    pub spend: Option<Spend<'a>>,
    /// The latest preamble's budget, in US dollars; none when unset.
    pub budget: Option<f64>,
    /// When the session's prompt cache expires, as `ts`; none when unset.
    pub cache_expires: Option<u64>,
    /// The last reply's output speed, in tokens per second.
    pub speed: Option<u64>,
    /// How many turns have completed.
    pub turns: u64,
    /// The MCP servers down, by name.
    pub tools_down: &'a std::collections::BTreeSet<String>,
    /// How many sessions wait; none at zero.
    pub waiting: Option<usize>,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// One changed file: its path with its added and removed lines.
#[derive(Debug, Clone, Copy)]
pub struct ChangedFile<'a> {
    /// The file's path.
    pub path: &'a str,
    /// Lines added.
    pub added: u64,
    /// Lines removed.
    pub removed: u64,
}

/// The changed-files card: every changed file in the order the
/// built-in ranks them.
#[derive(Debug, Clone, Copy)]
pub struct ChangedFiles<'a> {
    /// Every changed file, ranked.
    pub files: &'a [ChangedFile<'a>],
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// One delegate the Delegates card draws: running delegates in job
/// start order first, then finished ones, unclamped.
#[derive(Debug, Clone)]
pub struct Delegate<'a> {
    /// The delegate's job.
    pub job_id: &'a str,
    /// The job's description.
    pub description: &'a str,
    /// The harness, such as `fiber`.
    pub harness: &'a str,
    /// The model reference, with any role resolved.
    pub model: &'a str,
    /// Tool calls the delegate has made so far; none known read as
    /// zero.
    pub calls: u64,
    /// The latest call's tool name and subject, after the first call.
    pub step: Option<&'a str>,
    /// When the delegate started, as `ts`; none until its status.
    pub started: Option<u64>,
    /// How it ended; none while it runs.
    pub outcome: Option<Outcome>,
    /// When it ended, as `ts`; none while it runs.
    pub ended: Option<u64>,
}

/// The Delegates card: its running and finished delegates.
#[derive(Debug, Clone)]
pub struct DelegatesCard<'a> {
    /// Running delegates in job start order first, then finished ones
    /// in job start order.
    pub delegates: Vec<Delegate<'a>>,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// One job the Jobs card lists: its id and description.
#[derive(Debug, Clone, Copy)]
pub struct Job<'a> {
    /// The job.
    pub job_id: &'a str,
    /// The job's description.
    pub description: &'a str,
}

/// The Jobs card: its running jobs with whether the person opened the
/// list.
#[derive(Debug, Clone, Copy)]
pub struct JobsCard<'a> {
    /// The running non-delegate jobs, in job start order.
    pub jobs: &'a [Job<'a>],
    /// Whether the person opened the jobs list.
    pub open: bool,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// An extension widget's card: its lines with whether the person opened
/// it.
#[derive(Debug, Clone, Copy)]
pub struct WidgetCard<'a> {
    /// The extension's name.
    pub extension: &'a str,
    /// The widget's id.
    pub widget: &'a str,
    /// Its latest lines, in arrival order.
    pub lines: &'a [String],
    /// Whether the person opened the widget.
    pub open: bool,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// One panel card's source data: the five tabled inputs.
#[derive(Debug, Clone)]
pub enum Card<'a> {
    /// The attached session: `card:session`.
    Session(SessionCard<'a>),
    /// Every changed file, ranked: `card:changed_files`.
    ChangedFiles(ChangedFiles<'a>),
    /// Running then finished delegates: `card:delegates`.
    Delegates(DelegatesCard<'a>),
    /// The running jobs: `card:jobs`.
    Jobs(JobsCard<'a>),
    /// An extension widget's lines: `card:<extension>/<widget>`.
    Widget(WidgetCard<'a>),
}

/// The narrow layout's status rows: the cards it shows with what each
/// one reads.
#[derive(Debug, Clone)]
pub struct StatusRows<'a> {
    /// The cards the narrow layout shows, in order: `session`,
    /// `changed_files`, `delegates`, `jobs`, `quota`, or an extension
    /// widget's `extension/widget`.
    pub cards: Vec<String>,
    /// The same input `card:session` draws.
    pub session: SessionCard<'a>,
    /// The same input `card:changed_files` draws.
    pub changed_files: ChangedFiles<'a>,
    /// How many delegates run.
    pub delegates_running: usize,
    /// How many jobs run, delegates excluded.
    pub jobs_running: usize,
    /// The extension widgets on screen, with every line.
    pub widgets: Vec<WidgetCard<'a>>,
    /// How many sessions wait; none at zero.
    pub waiting: Option<usize>,
    /// The quota answer; none when the built-in draws no Quota segment.
    pub quota: Option<Quota<'a>>,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// Hashes an `f64` by its bits, so equal floats key equal.
fn hash_float(value: f64, key: &mut DefaultHasher) {
    value.to_bits().hash(key);
}

impl WorkingLine {
    /// The cache key over the slot and every field.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.started.hash(&mut key);
        self.retry.is_some().hash(&mut key);
        if let Some(retry) = &self.retry {
            crate::format::code(&retry.code).hash(&mut key);
            retry.attempt.hash(&mut key);
            retry.last_attempt.hash(&mut key);
            retry.delay_ms.hash(&mut key);
            retry.ts.hash(&mut key);
        }
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl<'a> SteeringMessage<'a> {
    /// Hashes the message's every field, images element-wise.
    fn hash_key(&self, key: &mut DefaultHasher) {
        self.text.hash(key);
        hash_images(&self.images, key);
        self.droppable.hash(key);
        self.selectable.hash(key);
        self.selected.hash(key);
    }
}

impl SteeringQueue<'_> {
    /// The cache key over the slot and every field, messages oldest
    /// first.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.messages.len().hash(&mut key);
        for message in &self.messages {
            message.hash_key(&mut key);
        }
        self.flat.hash(&mut key);
        key.finish()
    }
}

/// Hashes a spend's every field: the status spend whole, or the
/// attached fallback's cost alone.
fn hash_spend(spend: &Spend<'_>, key: &mut DefaultHasher) {
    match spend {
        Spend::Status(status) => {
            0u8.hash(key);
            match status.cost {
                Some(cost) => {
                    true.hash(key);
                    hash_float(cost, key);
                }
                None => false.hash(key),
            }
            status.tokens.input.hash(key);
            status.tokens.output.hash(key);
            status.tokens.cache_read.hash(key);
            status.tokens.cache_write.hash(key);
            hash_float(status.subscription_cost, key);
        }
        Spend::Attached { cost } => {
            1u8.hash(key);
            hash_float(*cost, key);
        }
    }
}

/// Hashes a quota answer's every field: its rows in order with the
/// session's threshold.
fn hash_quota(quota: &Quota<'_>, key: &mut DefaultHasher) {
    quota.rows.len().hash(key);
    for row in quota.rows {
        row.provider.hash(key);
        row.credential.hash(key);
        row.windows.is_some().hash(key);
        if let Some(windows) = &row.windows {
            windows.len().hash(key);
            for window in windows {
                window.name.hash(key);
                match window.percent_used {
                    Some(share) => {
                        true.hash(key);
                        hash_float(share, key);
                    }
                    None => false.hash(key),
                }
                window.reset.is_some().hash(key);
                window.resets_at.hash(key);
            }
        }
        row.credit.is_some().hash(key);
        if let Some(credit) = &row.credit {
            hash_float(credit.remaining, key);
            match credit.limit {
                Some(limit) => {
                    true.hash(key);
                    hash_float(limit, key);
                }
                None => false.hash(key),
            }
        }
        row.unreachable.is_some().hash(key);
    }
    hash_float(quota.notice_at, key);
}

impl SessionCard<'_> {
    /// The cache key over the slot and every field.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.workspace.hash(&mut key);
        self.branch.is_some().hash(&mut key);
        if let Some(branch) = &self.branch {
            match branch {
                BranchName::Named(name) => {
                    0u8.hash(&mut key);
                    name.hash(&mut key);
                }
                BranchName::Detached => 1u8.hash(&mut key),
            }
        }
        self.model.hash(&mut key);
        self.thinking.hash(&mut key);
        self.thinking_now.hash(&mut key);
        self.context.is_some().hash(&mut key);
        if let Some(context) = &self.context {
            context.tokens.hash(&mut key);
            context.window.hash(&mut key);
            context.trigger_at.hash(&mut key);
        }
        self.cache_expires.hash(&mut key);
        self.spend.is_some().hash(&mut key);
        if let Some(spend) = &self.spend {
            hash_spend(spend, &mut key);
        }
        self.budget.is_some().hash(&mut key);
        if let Some(budget) = self.budget {
            hash_float(budget, &mut key);
        }
        self.speed.hash(&mut key);
        self.turns.hash(&mut key);
        self.tools_down.hash(&mut key);
        self.waiting.hash(&mut key);
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl ChangedFiles<'_> {
    /// The cache key over the slot and every file, in ranked order.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.files.len().hash(&mut key);
        for file in self.files {
            file.path.hash(&mut key);
            file.added.hash(&mut key);
            file.removed.hash(&mut key);
        }
        self.flat.hash(&mut key);
        key.finish()
    }
}

/// Hashes a delegate outcome's variant.
fn hash_outcome(outcome: Outcome, key: &mut DefaultHasher) {
    match outcome {
        Outcome::Completed => 0u8.hash(key),
        Outcome::Failed => 1u8.hash(key),
        Outcome::Cancelled => 2u8.hash(key),
    }
}

impl Delegate<'_> {
    /// Hashes the delegate's every field.
    fn hash_key(&self, key: &mut DefaultHasher) {
        self.job_id.hash(key);
        self.description.hash(key);
        self.harness.hash(key);
        self.model.hash(key);
        self.calls.hash(key);
        self.step.hash(key);
        self.started.hash(key);
        self.outcome.is_some().hash(key);
        if let Some(outcome) = self.outcome {
            hash_outcome(outcome, key);
        }
        self.ended.hash(key);
    }
}

impl DelegatesCard<'_> {
    /// The cache key over the slot and every delegate, running then
    /// finished.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.delegates.len().hash(&mut key);
        for delegate in &self.delegates {
            delegate.hash_key(&mut key);
        }
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl JobsCard<'_> {
    /// The cache key over the slot and every job, in start order.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.jobs.len().hash(&mut key);
        for job in self.jobs {
            job.job_id.hash(&mut key);
            job.description.hash(&mut key);
        }
        self.open.hash(&mut key);
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl WidgetCard<'_> {
    /// The cache key over the slot and every field.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.extension.hash(&mut key);
        self.widget.hash(&mut key);
        self.lines.hash(&mut key);
        self.open.hash(&mut key);
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl Card<'_> {
    /// The cache key over the slot and every field.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        match self {
            Card::Session(card) => card.key(slot),
            Card::ChangedFiles(card) => card.key(slot),
            Card::Delegates(card) => card.key(slot),
            Card::Jobs(card) => card.key(slot),
            Card::Widget(card) => card.key(slot),
        }
    }

    /// Whether the screen reader drops every style from the returned
    /// spans.
    pub(crate) fn flat(&self) -> bool {
        match self {
            Card::Session(card) => card.flat,
            Card::ChangedFiles(card) => card.flat,
            Card::Delegates(card) => card.flat,
            Card::Jobs(card) => card.flat,
            Card::Widget(card) => card.flat,
        }
    }
}

impl StatusRows<'_> {
    /// The cache key over the slot and every field, cards in order.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.cards.hash(&mut key);
        // The session input keys itself; hashing the key keeps the
        // session and card keys over the same fields.
        self.session.key(slot).hash(&mut key);
        self.changed_files.key(slot).hash(&mut key);
        self.delegates_running.hash(&mut key);
        self.jobs_running.hash(&mut key);
        self.widgets.len().hash(&mut key);
        for widget in &self.widgets {
            widget.key(slot).hash(&mut key);
        }
        self.waiting.hash(&mut key);
        self.quota.is_some().hash(&mut key);
        if let Some(quota) = &self.quota {
            hash_quota(quota, &mut key);
        }
        self.flat.hash(&mut key);
        key.finish()
    }
}
