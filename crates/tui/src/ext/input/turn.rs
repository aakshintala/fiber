//! A turn's card whole: its prompts, every entry, and its ending,
//! with the cache keys over the card and every field.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::Value;

use contract::ErrorCode;
use contract::events::FileChange;
use contract::shapes::Usage;

use super::slots::{PromptImage, RowError, Status, hash_images, hash_value};

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

impl<'a> TurnThought<'a> {
    /// Hashes the thought's every field.
    pub(crate) fn hash_key(&self, key: &mut DefaultHasher) {
        self.text.hash(key);
        self.started.hash(key);
        self.ended.hash(key);
        self.open.hash(key);
    }
}

impl<'a> TurnStreaming<'a> {
    /// Hashes the in-flight call's every field.
    pub(crate) fn hash_key(&self, key: &mut DefaultHasher) {
        self.message.hash(key);
        self.index.hash(key);
        self.name.hash(key);
        self.text.hash(key);
    }
}

impl<'a> TurnFailed<'a> {
    /// Hashes the retried call's code and attempt.
    pub(crate) fn hash_key(&self, key: &mut DefaultHasher) {
        self.code.hash(key);
        self.attempt.hash(key);
    }
}

impl<'a> TurnCall<'a> {
    /// Hashes the call's every field, nested fields element-wise.
    pub(crate) fn hash_key(&self, key: &mut DefaultHasher) {
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
pub(crate) fn hash_band_state(state: &TurnBandState, key: &mut DefaultHasher) {
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
