//! One slot's renderer input: everything its built-in renderer reads,
//! folded from the event stream, with the cache key over the slot and
//! every field. No slot sees the terminal's internals (`docs/tui.md`,
//! "Extension seams").

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::Value;

use contract::ErrorCode;
use contract::events::FileChange;
use contract::shapes::{ProviderFailure, Usage};

/// What a slot receives: everything its built-in renderer reads, folded
/// from the event stream. No slot sees the terminal's internals.
#[derive(Debug, Clone)]
pub enum Input<'a> {
    /// A ledger row's call, without the step's gutter.
    LedgerRow(LedgerRow<'a>),
    /// A turn's card whole: its prompts, every entry, and its ending.
    TurnCard(TurnCard<'a>),
    /// A person's prompt bubble with its images.
    Prompt(Prompt<'a>),
    /// An assistant reply's text.
    Reply(ReplyText<'a>),
    /// One thinking line.
    ThinkingLine(ThinkingLine<'a>),
    /// A tool group's summary line.
    GroupLine(GroupLine<'a>),
    /// A steering message where it landed, and when.
    Steer(Steer<'a>),
    /// What a form left: the answers or the decline.
    Answers(Answers<'a>),
    /// A handoff's band without its note.
    HandoffBand(HandoffBand<'a>),
    /// A turn's failure line.
    ErrorLine(ErrorLine<'a>),
    /// One notice, wherever the built-in draws one.
    Notice(Notice<'a>),
}

/// One prompt image as its one line reads it: the file's name and its
/// size in pixels. The log's type never reaches the line.
#[derive(Debug, Clone, Copy)]
pub struct PromptImage<'a> {
    /// The file's path in the session's `artifacts/`.
    pub path: &'a str,
    /// The image's width in pixels.
    pub width: u32,
    /// The image's height in pixels.
    pub height: u32,
}

/// A person's prompt: its text and its images, in order.
#[derive(Debug, Clone)]
pub struct Prompt<'a> {
    /// The prompt's text.
    pub text: &'a str,
    /// Its images, in order.
    pub images: Vec<PromptImage<'a>>,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// An assistant reply: its text, Markdown included.
#[derive(Debug, Clone)]
pub struct ReplyText<'a> {
    /// The reply's text, Markdown included.
    pub text: &'a str,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// One thinking line: its text, when it ran, and whether it is open.
#[derive(Debug, Clone)]
pub struct ThinkingLine<'a> {
    /// The thought's text.
    pub text: &'a str,
    /// When the thought started.
    pub started: u64,
    /// When it ended; none while it streams.
    pub ended: Option<u64>,
    /// Whether the thought is open.
    pub open: bool,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// One of a card's prompts: its text and its images, in order.
#[derive(Debug, Clone)]
pub struct CardPrompt<'a> {
    /// The prompt's text.
    pub text: &'a str,
    /// Its images, in order.
    pub images: Vec<PromptImage<'a>>,
}

/// One thinking block inlined in a card's group: what the line reads.
#[derive(Debug, Clone)]
pub struct TurnThought<'a> {
    /// The thought's text.
    pub text: &'a str,
    /// When the thought started.
    pub started: u64,
    /// When it ended; none while it streams.
    pub ended: Option<u64>,
    /// Whether the thought is open.
    pub open: bool,
}

/// One in-flight model call a card's group summary names.
#[derive(Debug, Clone)]
pub struct TurnStreaming<'a> {
    /// The message whose arguments are still arriving.
    pub message: &'a str,
    /// The call's position in its message.
    pub index: u32,
    /// The tool's name, when announced.
    pub name: Option<&'a str>,
    /// The raw arguments so far.
    pub text: &'a str,
}

/// One retried model call a card's section names: its code and attempt.
#[derive(Debug, Clone)]
pub struct TurnFailed<'a> {
    /// The failure's code.
    pub code: &'a str,
    /// The attempt that failed.
    pub attempt: u32,
}

/// One tool call inlined in a card's group: everything the ledger row
/// reads, with the opened detail an open call reads.
#[derive(Debug, Clone)]
pub struct TurnCall<'a> {
    /// The tool's name.
    pub name: &'a str,
    /// The call's arguments.
    pub arguments: &'a Value,
    /// What the call is doing, or how it ended.
    pub status: Status,
    /// The process's exit code, when it ran one.
    pub exit: Option<i32>,
    /// The completion's timestamp less the start's, in milliseconds;
    /// none while the call runs and none when it never started.
    pub duration_ms: Option<u64>,
    /// How many lines the result's text parts hold.
    pub lines: u64,
    /// The file counts of an edit or write.
    pub changes: &'a [FileChange],
    /// A failed call's code and message, split from its headline.
    pub error: Option<RowError<'a>>,
    /// The first non-empty line of the result's text, else "done".
    pub output: &'a str,
    /// The call's headline: a failed call's "<code>: <message>" or a
    /// denial's reason, else empty.
    pub headline: &'a str,
    /// The result's text parts joined with "\n".
    pub content: &'a str,
    /// The suffix a form's answer replaces the status with.
    pub answer: Option<&'a str>,
    /// The image parts of its result, in order.
    pub images: Vec<PromptImage<'a>>,
    /// Whether the call is open.
    pub open: bool,
    /// Whether a `permission_requested` for it is open.
    pub asking: bool,
}

/// One step's part of a card's group inlined: its thoughts, calls and
/// retried model calls.
#[derive(Debug, Clone)]
pub struct TurnSection<'a> {
    /// The step.
    pub step: u64,
    /// Its thinking blocks, in order.
    pub thoughts: Vec<TurnThought<'a>>,
    /// Its tool calls, in order.
    pub calls: Vec<TurnCall<'a>>,
    /// Its retried model calls, in order.
    pub failed: Vec<TurnFailed<'a>>,
}

/// One tool group inlined in a card: the summary line's fields with the
/// ledger's sections.
#[derive(Debug, Clone)]
pub struct TurnGroup<'a> {
    /// The earliest timestamp the group spans.
    pub first: u64,
    /// The latest timestamp the group spans.
    pub last: u64,
    /// Whether the turn runs and this is its open group.
    pub running: bool,
    /// Whether the group's ledger is open.
    pub open: bool,
    /// Whether its turn was cut short.
    pub cut: bool,
    /// Its steps, in order.
    pub sections: Vec<TurnSection<'a>>,
    /// The calls the model is still emitting.
    pub streaming: Vec<TurnStreaming<'a>>,
}

/// A line from outside a turn, as drawn: joined span text, a warning's
/// message, or orphaned jobs with their open state.
#[derive(Debug, Clone)]
pub enum TurnAside<'a> {
    /// One line, its spans joined.
    Line {
        /// The joined span text.
        text: String,
    },
    /// A failed MCP server's message.
    Warning {
        /// The message.
        message: &'a str,
    },
    /// The jobs a resumed process marked orphaned.
    Orphans {
        /// Each one's name and message, in order.
        jobs: Vec<TurnOrphan<'a>>,
        /// Whether the jobs are open.
        open: bool,
    },
}

/// One orphaned job: its name and message.
#[derive(Debug, Clone)]
pub struct TurnOrphan<'a> {
    /// The job's name.
    pub name: &'a str,
    /// The job's message.
    pub message: &'a str,
}

/// A handoff's band: what the band row reads. The context sizes ride
/// [`TurnBandState::Done`], so only a done band carries them.
#[derive(Debug, Clone)]
pub struct TurnBand<'a> {
    /// What the handoff runs at.
    pub trigger: &'a str,
    /// The band's timestamp.
    pub ts: u64,
    /// What the band is doing, or how it ended.
    pub state: TurnBandState,
}

/// What a handoff's band is doing, or how it ended. The context sizes
/// ride the done state, so a state without them cannot carry them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnBandState {
    /// The note is being written.
    Writing,
    /// The handoff completed, with the context sizes.
    Done(TurnSizes),
    /// It failed or was cancelled: the context is unchanged.
    Unchanged {
        /// Whether a cancel ended it rather than a failure.
        cancelled: bool,
    },
}

/// A completed handoff's context sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnSizes {
    /// The context size before.
    pub before: u64,
    /// The context size after, once the first request after it returns.
    pub after: Option<u64>,
}

/// One answered row in a card's group entry.
#[derive(Debug, Clone)]
pub struct TurnAnswerPart<'a> {
    /// The question's header.
    pub header: &'a str,
    /// The answer's text, or "skipped".
    pub text: &'a str,
    /// Whether the question was skipped.
    pub skipped: bool,
}

/// What a form left in a card: the answers or the decline.
#[derive(Debug, Clone)]
pub enum TurnAnswers<'a> {
    /// The person answered it: the questions with the resolution's time.
    Given {
        /// Each question's part, in field order.
        parts: Vec<TurnAnswerPart<'a>>,
        /// The note on the whole form, when one was left.
        note: Option<&'a str>,
        /// When the form was resolved.
        ts: u64,
    },
    /// The person declined it.
    Declined,
}

/// One item in a card: every entry variant the built-in draws.
#[derive(Debug, Clone)]
pub enum TurnEntry<'a> {
    /// One text part of a reply, Markdown included.
    Reply {
        /// The reply's text, Markdown included.
        text: &'a str,
    },
    /// A steering message where it landed, and when.
    Steer {
        /// The message's text.
        text: &'a str,
        /// Its images, in order.
        images: Vec<PromptImage<'a>>,
        /// When it landed.
        ts: u64,
    },
    /// A tool group, inlined.
    Group(TurnGroup<'a>),
    /// A line that came while the turn ran.
    Aside(TurnAside<'a>),
    /// A handoff's band: what follows is the second card.
    Band(TurnBand<'a>),
    /// The person's answers to a question form.
    Answers(TurnAnswers<'a>),
}

/// How a card's turn ended, with its timestamp; a failure carries the
/// error the ✗ lines draw. Each outcome rides its own variant, so an
/// error attaches only to a failure.
#[derive(Debug, Clone)]
pub enum TurnEnded<'a> {
    /// The turn completed.
    Completed {
        /// When the turn ended.
        ended_ts: u64,
    },
    /// The turn was interrupted.
    Interrupted {
        /// When the turn ended.
        ended_ts: u64,
    },
    /// The turn failed.
    Failed {
        /// When the turn ended.
        ended_ts: u64,
        /// The failure's code and message; none when the event named
        /// none.
        error: Option<TurnError<'a>>,
    },
    /// Fiber stopped before the turn completed.
    CutShort {
        /// When the turn ended.
        ended_ts: u64,
    },
}

/// A failed turn's error: its code and message.
#[derive(Debug, Clone)]
pub struct TurnError<'a> {
    /// A stable label a consumer switches on.
    pub code: ErrorCode,
    /// Fiber's own sentence.
    pub message: &'a str,
}

/// A tool group's summary line: the aggregates its built-in reads,
/// flattened across the group's sections.
#[derive(Debug, Clone)]
pub struct GroupLine<'a> {
    /// Whether the turn runs and this is its open group.
    pub running: bool,
    /// The group's calls, in section order, as the card's group entries
    /// carry them.
    pub calls: Vec<TurnCall<'a>>,
    /// The calls the model is still emitting.
    pub streaming: Vec<TurnStreaming<'a>>,
    /// The group's thinking blocks, in order.
    pub thoughts: Vec<TurnThought<'a>>,
    /// Its retried model calls, in order.
    pub failed: Vec<TurnFailed<'a>>,
    /// The earliest timestamp the group spans.
    pub first: u64,
    /// The latest timestamp the group spans.
    pub last: u64,
    /// Whether the group's ledger is open.
    pub open: bool,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// A steering message: its text with its images, and when it landed.
#[derive(Debug, Clone)]
pub struct Steer<'a> {
    /// The message's text.
    pub text: &'a str,
    /// Its images, in order.
    pub images: Vec<PromptImage<'a>>,
    /// When it landed.
    pub ts: u64,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// What a form left: the answers with the resolution's time, or the
/// decline. Each state's payload rides [`TurnAnswers`], so a decline
/// carries no answer parts.
#[derive(Debug, Clone)]
pub struct Answers<'a> {
    /// The answers, or the decline.
    pub answers: TurnAnswers<'a>,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// A handoff's band without its note: what the band row reads. The
/// context sizes ride [`TurnBandState::Done`], so only a done band
/// carries them.
#[derive(Debug, Clone)]
pub struct HandoffBand<'a> {
    /// What the handoff runs at.
    pub trigger: &'a str,
    /// The band's timestamp.
    pub ts: u64,
    /// What the band is doing, or how it ended.
    pub state: TurnBandState,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// A turn's failure line: the code and message the ✗ line draws, with
/// the provider's words when one failed.
#[derive(Debug, Clone)]
pub struct ErrorLine<'a> {
    /// A stable label a consumer switches on.
    pub code: ErrorCode,
    /// Fiber's own sentence.
    pub message: &'a str,
    /// The provider's name, HTTP status and message, when one failed.
    pub provider: Option<&'a ProviderFailure>,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// One notice: its registry code and message.
#[derive(Debug, Clone)]
pub struct Notice<'a> {
    /// Its registry code.
    pub code: &'a str,
    /// Its message.
    pub text: &'a str,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// A turn's card whole: its prompts, every entry, and its ending.
#[derive(Debug, Clone)]
pub struct TurnCard<'a> {
    /// The turn's prompts, in order.
    pub prompts: Vec<CardPrompt<'a>>,
    /// The card's entries, in order.
    pub entries: Vec<TurnEntry<'a>>,
    /// When the turn started.
    pub started: u64,
    /// How many model calls the turn made.
    pub calls: u64,
    /// The turn's totals: tokens, cost and subscription cost.
    pub usage: Usage,
    /// How the turn ended; none while it runs.
    pub ended: Option<TurnEnded<'a>>,
    /// Whether the turn is still running.
    pub running: bool,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// A ledger row's call: the call's name, arguments, status, exit code,
/// duration, line count, `changes` and error, the first line of the
/// result's text, a form answer's suffix, and whether the screen reader
/// drops every style.
#[derive(Debug, Clone, Copy)]
pub struct LedgerRow<'a> {
    /// The tool's name.
    pub name: &'a str,
    /// The call's arguments.
    pub arguments: &'a Value,
    /// What the call is doing, or how it ended.
    pub status: Status,
    /// The process's exit code, when it ran one.
    pub exit: Option<i32>,
    /// The completion's timestamp less the start's, in milliseconds;
    /// none while the call runs and none when it never started.
    pub duration_ms: Option<u64>,
    /// How many lines the result's text parts hold.
    pub lines: u64,
    /// The file counts of an edit or write.
    pub changes: &'a [FileChange],
    /// A failed call's code and message, split from its headline.
    pub error: Option<RowError<'a>>,
    /// The first non-empty line of the result's text, else "done".
    pub output: &'a str,
    /// The suffix a form's answer replaces the status with.
    pub answer: Option<&'a str>,
    /// The screen reader drops every style from the returned spans.
    pub flat: bool,
}

/// A failed call's error: its headline's code and message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowError<'a> {
    /// The headline before the first ": ", else empty.
    pub code: &'a str,
    /// The headline after the first ": ", else the whole headline.
    pub message: &'a str,
}

/// What a ledger row's call is doing, or how it ended: the built-in row
/// draws pending and running alike, so only an extension tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// No `tool_call_started` and no completion.
    Pending,
    /// Started and not completed.
    Running,
    /// It completed.
    Completed,
    /// It failed.
    Failed,
    /// It was denied.
    Denied,
    /// It was cancelled.
    Cancelled,
    /// Started on a cut turn: its outcome is unknown.
    Unknown,
}

impl<'a> LedgerRow<'a> {
    /// The cache key over the slot and every field: the entry key adds
    /// the width, the failure key does not.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.name.hash(&mut key);
        hash_value(self.arguments, &mut key);
        (self.status as u8).hash(&mut key);
        self.exit.hash(&mut key);
        self.duration_ms.hash(&mut key);
        self.lines.hash(&mut key);
        self.changes.len().hash(&mut key);
        for change in self.changes {
            change.path.hash(&mut key);
            change.added.hash(&mut key);
            change.removed.hash(&mut key);
        }
        self.error.map(|error| error.code).hash(&mut key);
        self.error.map(|error| error.message).hash(&mut key);
        self.output.hash(&mut key);
        self.answer.hash(&mut key);
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl<'a> PromptImage<'a> {
    /// Hashes the image's every field.
    fn hash_key(&self, key: &mut DefaultHasher) {
        self.path.hash(key);
        self.width.hash(key);
        self.height.hash(key);
    }
}

/// Hashes images element-wise, in order.
fn hash_images(images: &[PromptImage<'_>], key: &mut DefaultHasher) {
    images.len().hash(key);
    for image in images {
        image.hash_key(key);
    }
}

impl<'a> Prompt<'a> {
    /// The cache key over the slot and every field.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.text.hash(&mut key);
        hash_images(&self.images, &mut key);
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl<'a> ReplyText<'a> {
    /// The cache key over the slot and every field.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.text.hash(&mut key);
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl<'a> ThinkingLine<'a> {
    /// The cache key over the slot and every field.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.text.hash(&mut key);
        self.started.hash(&mut key);
        self.ended.hash(&mut key);
        self.open.hash(&mut key);
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl<'a> TurnThought<'a> {
    /// Hashes the thought's every field.
    fn hash_key(&self, key: &mut DefaultHasher) {
        self.text.hash(key);
        self.started.hash(key);
        self.ended.hash(key);
        self.open.hash(key);
    }
}

impl<'a> TurnStreaming<'a> {
    /// Hashes the in-flight call's every field.
    fn hash_key(&self, key: &mut DefaultHasher) {
        self.message.hash(key);
        self.index.hash(key);
        self.name.hash(key);
        self.text.hash(key);
    }
}

impl<'a> TurnFailed<'a> {
    /// Hashes the retried call's code and attempt.
    fn hash_key(&self, key: &mut DefaultHasher) {
        self.code.hash(key);
        self.attempt.hash(key);
    }
}

impl<'a> TurnCall<'a> {
    /// Hashes the call's every field, nested fields element-wise.
    fn hash_key(&self, key: &mut DefaultHasher) {
        self.name.hash(key);
        hash_value(self.arguments, key);
        (self.status as u8).hash(key);
        self.exit.hash(key);
        self.duration_ms.hash(key);
        self.lines.hash(key);
        self.changes.len().hash(key);
        for change in self.changes {
            change.path.hash(key);
            change.added.hash(key);
            change.removed.hash(key);
        }
        self.error.map(|error| error.code).hash(key);
        self.error.map(|error| error.message).hash(key);
        self.output.hash(key);
        self.headline.hash(key);
        self.content.hash(key);
        self.answer.hash(key);
        hash_images(&self.images, key);
        self.open.hash(key);
        self.asking.hash(key);
    }
}

impl<'a> TurnSection<'a> {
    /// Hashes the section's every field, nested fields element-wise.
    fn hash_key(&self, key: &mut DefaultHasher) {
        self.step.hash(key);
        self.thoughts.len().hash(key);
        for thought in &self.thoughts {
            thought.hash_key(key);
        }
        self.calls.len().hash(key);
        for call in &self.calls {
            call.hash_key(key);
        }
        self.failed.len().hash(key);
        for failed in &self.failed {
            failed.hash_key(key);
        }
    }
}

impl<'a> TurnGroup<'a> {
    /// Hashes the group's every field, nested fields element-wise.
    fn hash_key(&self, key: &mut DefaultHasher) {
        self.first.hash(key);
        self.last.hash(key);
        self.running.hash(key);
        self.open.hash(key);
        self.cut.hash(key);
        self.sections.len().hash(key);
        for section in &self.sections {
            section.hash_key(key);
        }
        self.streaming.len().hash(key);
        for streaming in &self.streaming {
            streaming.hash_key(key);
        }
    }
}

impl<'a> TurnAside<'a> {
    /// Hashes the aside's every field.
    fn hash_key(&self, key: &mut DefaultHasher) {
        match self {
            Self::Line { text } => {
                0u8.hash(key);
                text.hash(key);
            }
            Self::Warning { message } => {
                1u8.hash(key);
                message.hash(key);
            }
            Self::Orphans { jobs, open } => {
                2u8.hash(key);
                jobs.len().hash(key);
                for job in jobs {
                    job.name.hash(key);
                    job.message.hash(key);
                }
                open.hash(key);
            }
        }
    }
}

impl TurnBand<'_> {
    /// Hashes the band's every field.
    fn hash_key(&self, key: &mut DefaultHasher) {
        self.trigger.hash(key);
        self.ts.hash(key);
        hash_band_state(&self.state, key);
    }
}

/// Hashes a band state with its payload: the sizes ride `done`.
fn hash_band_state(state: &TurnBandState, key: &mut DefaultHasher) {
    match state {
        TurnBandState::Writing => 0u8.hash(key),
        TurnBandState::Done(sizes) => {
            1u8.hash(key);
            sizes.before.hash(key);
            sizes.after.hash(key);
        }
        TurnBandState::Unchanged { cancelled } => {
            2u8.hash(key);
            cancelled.hash(key);
        }
    }
}

impl<'a> TurnEntry<'a> {
    /// Hashes the entry's every field, nested fields element-wise.
    fn hash_key(&self, key: &mut DefaultHasher) {
        match self {
            Self::Reply { text } => {
                0u8.hash(key);
                text.hash(key);
            }
            Self::Steer { text, images, ts } => {
                1u8.hash(key);
                text.hash(key);
                hash_images(images, key);
                ts.hash(key);
            }
            Self::Group(group) => {
                2u8.hash(key);
                group.hash_key(key);
            }
            Self::Aside(aside) => {
                3u8.hash(key);
                aside.hash_key(key);
            }
            Self::Band(band) => {
                4u8.hash(key);
                band.hash_key(key);
            }
            Self::Answers(answers) => {
                5u8.hash(key);
                match answers {
                    TurnAnswers::Given { parts, note, ts } => {
                        0u8.hash(key);
                        parts.len().hash(key);
                        for part in parts {
                            part.header.hash(key);
                            part.text.hash(key);
                            part.skipped.hash(key);
                        }
                        note.hash(key);
                        ts.hash(key);
                    }
                    TurnAnswers::Declined => 1u8.hash(key),
                }
            }
        }
    }
}

impl<'a> TurnCard<'a> {
    /// The cache key over the slot and every field, nested fields
    /// element-wise.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.prompts.len().hash(&mut key);
        for prompt in &self.prompts {
            prompt.text.hash(&mut key);
            hash_images(&prompt.images, &mut key);
        }
        self.entries.len().hash(&mut key);
        for entry in &self.entries {
            entry.hash_key(&mut key);
        }
        self.started.hash(&mut key);
        self.calls.hash(&mut key);
        self.usage.tokens.input.hash(&mut key);
        self.usage.tokens.cache_read.hash(&mut key);
        self.usage.tokens.cache_write.len().hash(&mut key);
        for (class, count) in &self.usage.tokens.cache_write {
            class.hash(&mut key);
            count.hash(&mut key);
        }
        self.usage.tokens.output.hash(&mut key);
        self.usage.cost.map(f64::to_bits).hash(&mut key);
        self.usage.subscription_cost.to_bits().hash(&mut key);
        self.ended.is_some().hash(&mut key);
        if let Some(ended) = &self.ended {
            match ended {
                TurnEnded::Completed { ended_ts } => {
                    0u8.hash(&mut key);
                    ended_ts.hash(&mut key);
                }
                TurnEnded::Interrupted { ended_ts } => {
                    1u8.hash(&mut key);
                    ended_ts.hash(&mut key);
                }
                TurnEnded::Failed { ended_ts, error } => {
                    2u8.hash(&mut key);
                    ended_ts.hash(&mut key);
                    error.is_some().hash(&mut key);
                    if let Some(error) = error {
                        crate::format::code(&error.code).hash(&mut key);
                        error.message.hash(&mut key);
                    }
                }
                TurnEnded::CutShort { ended_ts } => {
                    3u8.hash(&mut key);
                    ended_ts.hash(&mut key);
                }
            }
        }
        self.running.hash(&mut key);
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl<'a> GroupLine<'a> {
    /// The cache key over the slot and every field, nested fields
    /// element-wise.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.running.hash(&mut key);
        self.calls.len().hash(&mut key);
        for call in &self.calls {
            call.hash_key(&mut key);
        }
        self.streaming.len().hash(&mut key);
        for streaming in &self.streaming {
            streaming.hash_key(&mut key);
        }
        self.thoughts.len().hash(&mut key);
        for thought in &self.thoughts {
            thought.hash_key(&mut key);
        }
        self.failed.len().hash(&mut key);
        for failed in &self.failed {
            failed.hash_key(&mut key);
        }
        self.first.hash(&mut key);
        self.last.hash(&mut key);
        self.open.hash(&mut key);
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl<'a> Steer<'a> {
    /// The cache key over the slot and every field.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.text.hash(&mut key);
        hash_images(&self.images, &mut key);
        self.ts.hash(&mut key);
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl Answers<'_> {
    /// The cache key over the slot and every field.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        match &self.answers {
            TurnAnswers::Given { parts, note, ts } => {
                0u8.hash(&mut key);
                parts.len().hash(&mut key);
                for part in parts {
                    part.header.hash(&mut key);
                    part.text.hash(&mut key);
                    part.skipped.hash(&mut key);
                }
                note.hash(&mut key);
                ts.hash(&mut key);
            }
            TurnAnswers::Declined => 1u8.hash(&mut key),
        }
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl HandoffBand<'_> {
    /// The cache key over the slot and every field.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.trigger.hash(&mut key);
        self.ts.hash(&mut key);
        hash_band_state(&self.state, &mut key);
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl<'a> ErrorLine<'a> {
    /// The cache key over the slot and every field.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        crate::format::code(&self.code).hash(&mut key);
        self.message.hash(&mut key);
        self.provider.is_some().hash(&mut key);
        if let Some(provider) = self.provider {
            provider.name.hash(&mut key);
            provider.status.hash(&mut key);
            provider.message.hash(&mut key);
        }
        self.flat.hash(&mut key);
        key.finish()
    }
}

impl<'a> Notice<'a> {
    /// The cache key over the slot and every field.
    pub(crate) fn key(&self, slot: &str) -> u64 {
        let mut key = DefaultHasher::new();
        slot.hash(&mut key);
        self.code.hash(&mut key);
        self.text.hash(&mut key);
        self.flat.hash(&mut key);
        key.finish()
    }
}

/// The hash of a JSON value with object keys sorted, so the key never
/// depends on the map's order.
fn hash_value(value: &Value, key: &mut DefaultHasher) {
    match value {
        Value::Null => 0u8.hash(key),
        Value::Bool(done) => {
            1u8.hash(key);
            done.hash(key);
        }
        Value::Number(count) => {
            2u8.hash(key);
            count.to_string().hash(key);
        }
        Value::String(text) => {
            3u8.hash(key);
            text.hash(key);
        }
        Value::Array(items) => {
            4u8.hash(key);
            items.len().hash(key);
            for item in items {
                hash_value(item, key);
            }
        }
        Value::Object(fields) => {
            5u8.hash(key);
            // Without `preserve_order` the map is key-ordered, so its
            // order never depends on insertion.
            fields.len().hash(key);
            for (name, field) in fields {
                name.hash(key);
                hash_value(field, key);
            }
        }
    }
}
