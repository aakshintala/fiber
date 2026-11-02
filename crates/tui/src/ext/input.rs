//! One slot's renderer input: everything its built-in renderer reads,
//! folded from the event stream, with the cache key over the slot and
//! every field. No slot sees the terminal's internals (`docs/tui.md`,
//! "Extension seams").

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
    /// The working line: how long the turn runs, or its wait to retry.
    WorkingLine(WorkingLine),
    /// The queued steering messages, oldest first.
    SteeringQueue(SteeringQueue<'a>),
    /// The narrow layout's status rows.
    StatusRows(StatusRows<'a>),
    /// The input box's status row.
    InputStatus(InputStatus<'a>),
    /// One panel card's source data.
    Card(Card<'a>),
}

mod chrome;
mod slots;
mod turn;

pub use chrome::{
    BranchName, Card, ChangedFile, ChangedFiles, Context, Delegate, DelegatesCard, InputStatus,
    Job, JobsCard, QueuedImage, Quota, SessionCard, SessionContext, Spend, StatusRows,
    SteeringMessage, SteeringQueue, WidgetCard, WorkingLine, WorkingRetry,
};

pub use slots::{
    Answers, ErrorLine, GroupLine, HandoffBand, LedgerRow, Notice, Prompt, PromptImage, ReplyText,
    RowError, Steer, ThinkingLine,
};
pub use turn::{
    AnswerDisposition, CallState, CardPrompt, LedgerResult, TurnAnswerPart, TurnAnswers, TurnAside,
    TurnBand, TurnBandState, TurnCall, TurnCallResult, TurnCard, TurnCardState, TurnEnded,
    TurnEntry, TurnError, TurnFailed, TurnGroup, TurnGroupState, TurnOrphan, TurnSection,
    TurnSizes, TurnStreaming, TurnThought,
};
