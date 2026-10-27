//! The renderer seam one slot draws through (`docs/tui.md`, "Extension seams").
//!
//! A slot's renderer draws what follows the slot's own chrome with the
//! width less that chrome, and returns lines of spans. `tui` owns the
//! trait and the span rules; `main` adapts the extensions VM to it, so
//! `tui` depends on no crate that holds the VM (`docs/architecture.md`,
//! "The call rules"). `tui` tests use a fake renderer.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;

use contract::ErrorCode;
use contract::events::FileChange;
use contract::shapes::Usage;

pub(crate) mod cache;

use cache::Cache;
pub(crate) mod spans;

/// The seam bound to one page's rows: the installed renderer, the
/// page's cache, the page, and whether the screen reader drops every
/// style from the returned spans.
#[derive(Debug, Clone)]
pub(crate) struct Bound {
    /// The installed renderer's slot on this thread, if any.
    pub(crate) handle: Handle,
    /// The page's cache, shared with the paging window so entries drop
    /// with their page.
    pub(crate) cache: Arc<Cache>,
    /// The page these rows draw on.
    pub(crate) page: cache::PageKey,
    /// The drawn page's session-half snapshot.
    pub(crate) ctx: SessionCtx,
    /// The screen reader drops every style from the returned spans.
    pub(crate) flat: bool,
}

/// The renderer seam's shared state: what every page's rows draw
/// through. `Pages` owns one; `bind` hands each page's rows the seam
/// bound to that page. The handle is shared, so one install reaches
/// every page; each page keeps its own cache entries.
#[derive(Debug, Clone, Default)]
pub(crate) struct Ext {
    /// The installed renderer's slot on this thread, if any.
    pub(crate) handle: Handle,
    /// The pages' caches, one entry set per page.
    pub(crate) cache: Arc<Cache>,
    /// The screen reader drops every style from the returned spans.
    pub(crate) flat: bool,
}

impl Ext {
    /// The seam one page's rows draw through.
    pub(crate) fn bind(&self, page: cache::PageKey, ctx: SessionCtx) -> Bound {
        Bound {
            handle: self.handle,
            cache: Arc::clone(&self.cache),
            page,
            ctx,
            flat: self.flat,
        }
    }
}

/// The drawn session and its latest complete session-half map.
#[derive(Debug, Clone)]
pub struct SessionCtx {
    /// The session receiving the callback.
    pub session_id: contract::SessionId,
    /// None until `extensions_loaded` has been observed.
    pub halves: Option<BTreeMap<String, String>>,
}

/// A session half's observed identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SessionVersion {
    /// No event observed yet.
    Unseen,
    /// The received event omitted this extension.
    NotLoaded,
    /// The received version tag.
    Version(String),
}

/// The compatible contenders' selection for a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotSelection {
    /// No compatible contender; try the catch-all.
    Absent,
    /// One selected extension.
    Draw {
        /// The winner's identity.
        extension: String,
    },
    /// A conflict; use the built-in without trying the catch-all.
    Suppressed,
}

/// A callback's failure family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// An item-local error or malformed spans.
    HookFailed,
    /// A timeout switched the VM off until reload.
    ExtensionFailed,
}

/// A render result carrying the actual extension's identity.
#[derive(Debug)]
pub enum Decision {
    /// Validatable lines from the selected extension.
    Draw {
        /// The extension that drew.
        extension: String,
        /// Its lines.
        lines: Vec<Vec<RawSpan>>,
    },
    /// A defensive fallback when the session does not match.
    NotForSession {
        /// The extension.
        extension: String,
        /// Its client version.
        tui_version: String,
        /// The observed session-half identity.
        half: SessionVersion,
    },
    /// A typed callback failure.
    Fail(Failure),
}

/// Builds terminal-thread renderers from current local files.
pub trait Load: Send + Sync {
    /// The replacement renderer (possibly empty) and load notices.
    fn load(&self) -> (Rc<dyn Renderer>, Vec<(String, String)>);
}

/// One extension's drawing of a slot: whether it fills a slot name, and
/// the lines for one input. The terminal calls it on its own thread, and
/// the renderer makes no host call.
pub trait Renderer: std::fmt::Debug {
    /// The extension that draws: the fallback notice names it.
    fn name(&self) -> &str;
    /// Whether the extension fills exactly this slot name, such as
    /// `ledger_row:shell`.
    fn fills(&self, slot: &str) -> bool;
    /// Selects compatible contenders before cache lookup.
    fn fills_for(&self, _ctx: &SessionCtx, slot: &str) -> SlotSelection {
        if self.fills(slot) {
            SlotSelection::Draw {
                extension: self.name().to_owned(),
            }
        } else {
            SlotSelection::Absent
        }
    }
    /// Drains producer-deduplicated selection and callback notices.
    // Replacing the empty default with `vec![]` is behaviorally identical:
    // both construct a new empty vector, so no test can tell them apart.
    #[cfg_attr(false, mutants::skip)]
    fn take_pending(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// The selected half identity, absent for client-only renderers.
    fn half(&self, _ctx: &SessionCtx, _extension: &str) -> Option<String> {
        None
    }
    /// Lines for `slot`; `builtin` returns the built-in renderer's lines
    /// for any input, so an extension that only adds a badge decorates
    /// the built-in rather than drawing the row again.
    fn render(
        &self,
        ctx: &SessionCtx,
        slot: &str,
        input: &Input<'_>,
        width: u16,
        builtin: &dyn Fn(&Input<'_>, u16) -> Vec<Vec<RawSpan>>,
    ) -> Decision;
}

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

/// A handoff's band without its note: what the band row reads.
#[derive(Debug, Clone)]
pub struct TurnBand<'a> {
    /// What the handoff runs at.
    pub trigger: &'a str,
    /// The band's timestamp.
    pub ts: u64,
    /// What the band is doing, or how it ended.
    pub state: TurnBandState,
    /// The context sizes, only once the handoff is done.
    pub sizes: Option<TurnSizes>,
}

/// What a handoff's band is doing, or how it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnBandState {
    /// The note is being written.
    Writing,
    /// The handoff completed.
    Done,
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

/// How a card's turn ended: the outcome with its timestamp, and on a
/// failure the error the ✗ lines draw.
#[derive(Debug, Clone)]
pub struct TurnEnded<'a> {
    /// The outcome as the ▣ line names it: "completed", "interrupted",
    /// "failed", or "cut short".
    pub outcome: &'a str,
    /// When the turn ended.
    pub ended_ts: u64,
    /// A failed turn's code and message.
    pub error: Option<TurnError<'a>>,
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

/// One span of a returned line: `text` is required, everything else is
/// optional. `fg` and `bg` name one of the theme's colour roles; `link`
/// makes the span a link, opened on click; `click` names a click target
/// whose handlers a later ticket builds, so it draws as nothing here.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RawSpan {
    /// The span's text: no control character.
    pub text: String,
    /// The foreground role's name.
    pub fg: Option<String>,
    /// The background role's name.
    pub bg: Option<String>,
    /// Bold.
    pub bold: bool,
    /// Dim.
    pub dim: bool,
    /// Italic.
    pub italic: bool,
    /// Underlined.
    pub underline: bool,
    /// The link destination.
    pub link: Option<String>,
    /// The click target's id.
    pub click: Option<String>,
}

/// Why a renderer did not draw: the extension's error, or the wrong
/// shape its spans returned. The item falls back to its built-in row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The notice family, preserved through caching.
    pub kind: FailureKind,
    /// The extension that failed.
    pub extension: String,
    /// The Lua error, or what the spans got wrong.
    pub message: String,
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

impl<'a> TurnBand<'a> {
    /// Hashes the band's every field.
    fn hash_key(&self, key: &mut DefaultHasher) {
        self.trigger.hash(key);
        self.ts.hash(key);
        match self.state {
            TurnBandState::Writing => 0u8.hash(key),
            TurnBandState::Done => 1u8.hash(key),
            TurnBandState::Unchanged { cancelled } => {
                2u8.hash(key);
                cancelled.hash(key);
            }
        }
        self.sizes.map(|sizes| sizes.before).hash(key);
        self.sizes.and_then(|sizes| sizes.after).hash(key);
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
            ended.outcome.hash(&mut key);
            ended.ended_ts.hash(&mut key);
            ended.error.is_some().hash(&mut key);
            if let Some(error) = &ended.error {
                crate::format::code(&error.code).hash(&mut key);
                error.message.hash(&mut key);
            }
        }
        self.running.hash(&mut key);
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

thread_local! {
    /// The installed renderers, one entry per install slot created on
    /// this thread. The Lua VM is not `Send` and runs on the terminal's
    /// thread, so `App` and `Pages` hold only the `Copy` slot below, and
    /// every entry stays on the thread that created it
    /// (`docs/architecture.md`, "The call rules").
    static SLOTS: RefCell<HashMap<u64, Option<Rc<dyn Renderer>>>> =
        RefCell::new(HashMap::new());
    /// The next install slot's id on this thread.
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

/// An installed renderer's slot: a plain integer, so `App` and `Pages`
/// stay `Send` while the renderer itself never leaves the terminal's
/// thread. A handle resolved on a thread without its install draws the
/// built-in row and does not panic.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Handle(Option<u64>);

impl Handle {
    /// A fresh install slot on this thread, empty until `set`.
    pub(crate) fn create() -> Self {
        let id = NEXT_ID.with(|next| {
            let id = next.get();
            next.set(id.saturating_add(1));
            id
        });
        SLOTS.with(|slots| {
            slots.borrow_mut().insert(id, None);
        });
        Self(Some(id))
    }

    /// Installs the renderer seen through this handle on this thread.
    /// No entry on this thread means no install: a handle created on
    /// another thread never installs there.
    pub(crate) fn set(self, renderer: Rc<dyn Renderer>) {
        let Some(id) = self.0 else {
            return;
        };
        SLOTS.with(|slots| {
            if let Ok(mut slots) = slots.try_borrow_mut()
                && let Some(entry) = slots.get_mut(&id)
            {
                *entry = Some(renderer);
            }
        });
    }

    /// The renderer installed through this handle on this thread, if
    /// any. None for no handle, for a thread without the entry, or
    /// while the install is empty. A failed borrow reads as none, so a
    /// renderer that calls back into the cache never panics here.
    pub(crate) fn get(self) -> Option<Rc<dyn Renderer>> {
        let id = self.0?;
        SLOTS.with(|slots| slots.try_borrow().ok()?.get(&id)?.clone())
    }
}

#[cfg(test)]
#[path = "ext_tests.rs"]
pub(crate) mod tests;
