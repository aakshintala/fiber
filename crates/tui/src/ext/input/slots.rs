//! One row's renderer input: the single-row slot structs, with the
//! cache keys over the slot and every field.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::Value;

use contract::ErrorCode;
use contract::events::FileChange;
use contract::shapes::ProviderFailure;

use super::turn::{
    TurnAnswers, TurnBandState, TurnCall, TurnFailed, TurnStreaming, TurnThought, hash_band_state,
};

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
pub(crate) fn hash_images(images: &[PromptImage<'_>], key: &mut DefaultHasher) {
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
pub(crate) fn hash_value(value: &Value, key: &mut DefaultHasher) {
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
