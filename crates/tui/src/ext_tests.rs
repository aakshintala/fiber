//! Tests for the seam types: the input key and the renderer handle.

use super::{Decision, Ext, FailureKind, SessionCtx, SlotSelection};
use super::{
    Failure, Handle, Input, LedgerRow, Prompt, PromptImage, RawSpan, Renderer, ReplyText, RowError,
    Status, ThinkingLine, TurnCard,
};
use contract::events::FileChange;
use std::cell::{Cell, RefCell};

/// Converts legacy fake render callbacks while preserving early error returns.
pub(crate) fn decide(
    extension: &str,
    call: impl FnOnce() -> Result<Vec<Vec<RawSpan>>, Failure>,
) -> Decision {
    match call() {
        Ok(lines) => Decision::Draw {
            extension: extension.into(),
            lines,
        },
        Err(failure) => Decision::Fail(failure),
    }
}

/// Session-aware fake shared by paging, fold and delegate regressions.
#[derive(Debug)]
pub(crate) struct SessionRenderer {
    pub(crate) label: String,
    pub(crate) gated: bool,
    pub(crate) failure: Option<FailureKind>,
    pub(crate) calls: RefCell<Vec<Option<String>>>,
    pending: RefCell<Vec<(String, String)>>,
    reported: Cell<bool>,
}
impl SessionRenderer {
    pub(crate) fn new(label: &str) -> Self {
        Self {
            label: label.into(),
            gated: false,
            failure: None,
            calls: RefCell::new(Vec::new()),
            pending: RefCell::new(Vec::new()),
            reported: Cell::new(false),
        }
    }
}
impl Renderer for SessionRenderer {
    fn name(&self) -> &str {
        "session-test"
    }
    fn fills(&self, slot: &str) -> bool {
        !self.label.is_empty() && slot == "ledger_row"
    }
    fn fills_for(&self, ctx: &SessionCtx, slot: &str) -> SlotSelection {
        if !self.fills(slot) {
            return SlotSelection::Absent;
        }
        if self.gated
            && ctx
                .halves
                .as_ref()
                .and_then(|map| map.get(self.name()))
                .is_none_or(|version| !version.starts_with("v1."))
        {
            if ctx.halves.is_some() && !self.reported.replace(true) {
                self.pending.borrow_mut().push((
                    "extension_incompatible".into(),
                    "session-test TUI v3.0.0: not loaded or incompatible".into(),
                ));
            }
            return SlotSelection::Absent;
        }
        SlotSelection::Draw {
            extension: self.name().into(),
        }
    }
    fn half(&self, ctx: &SessionCtx, _: &str) -> Option<String> {
        if self.gated {
            ctx.halves.as_ref()?.get(self.name()).cloned()
        } else {
            None
        }
    }
    fn take_pending(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.pending.borrow_mut())
    }
    fn render(
        &self,
        ctx: &SessionCtx,
        _: &str,
        _: &Input<'_>,
        _: u16,
        _: &dyn Fn(&Input<'_>, u16) -> Vec<Vec<RawSpan>>,
    ) -> Decision {
        self.calls
            .borrow_mut()
            .push(ctx.session_id.clone().map(|id| id.0));
        if let Some(kind) = self.failure {
            return Decision::Fail(Failure {
                kind,
                extension: self.name().into(),
                message: "fixture failure".into(),
            });
        }
        Decision::Draw {
            extension: self.name().into(),
            lines: vec![
                vec![RawSpan {
                    text: self.label.clone(),
                    ..RawSpan::default()
                }];
                3
            ],
        }
    }
}

/// Rebuilds a fresh terminal-thread renderer for command and startup tests.
pub(crate) struct TestLoad {
    pub(crate) calls: std::sync::atomic::AtomicUsize,
    pub(crate) enabled: std::sync::atomic::AtomicBool,
    pub(crate) label: &'static str,
}
impl TestLoad {
    pub(crate) fn new(label: &'static str) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            enabled: std::sync::atomic::AtomicBool::new(true),
            label,
        })
    }
}
impl super::Load for TestLoad {
    fn load(&self) -> (std::rc::Rc<dyn Renderer>, Vec<(String, String)>) {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let label = if self.enabled.load(std::sync::atomic::Ordering::SeqCst) {
            self.label
        } else {
            ""
        };
        (
            std::rc::Rc::new(SessionRenderer::new(label)),
            vec![("extension_failed".into(), "load fixture notice".into())],
        )
    }
}

#[derive(Debug)]
struct Fake;

impl Renderer for Fake {
    fn name(&self) -> &str {
        ""
    }

    fn fills(&self, _slot: &str) -> bool {
        false
    }

    fn render(
        &self,
        _ctx: &crate::ext::SessionCtx,
        _slot: &str,
        _input: &Input<'_>,
        _width: u16,
        _builtin: &dyn Fn(&Input<'_>, u16) -> Vec<Vec<RawSpan>>,
    ) -> crate::ext::Decision {
        crate::ext::tests::decide(self.name(), || -> Result<Vec<Vec<RawSpan>>, Failure> {
            Err(Failure {
                kind: crate::ext::FailureKind::HookFailed,
                extension: String::new(),
                message: String::new(),
            })
        })
    }
}

#[allow(clippy::too_many_arguments, reason = "one argument per keyed field")]
fn ledger<'a>(
    name: &'a str,
    arguments: &'a serde_json::Value,
    status: Status,
    exit: Option<i32>,
    duration_ms: Option<u64>,
    lines: u64,
    changes: &'a [FileChange],
    error: Option<RowError<'a>>,
    output: &'a str,
    answer: Option<&'a str>,
    flat: bool,
) -> LedgerRow<'a> {
    LedgerRow {
        name,
        arguments,
        status,
        exit,
        duration_ms,
        lines,
        changes,
        error,
        output,
        answer,
        flat,
    }
}

struct Base {
    arguments: serde_json::Value,
    changes: Vec<FileChange>,
}

/// The baseline input every key case varies once.
fn base() -> Base {
    Base {
        arguments: serde_json::json!({"command": "ls", "nested": {"depth": 1}}),
        changes: vec![
            FileChange {
                path: "a.rs".to_owned(),
                added: 1,
                removed: 2,
            },
            FileChange {
                path: "b.rs".to_owned(),
                added: 3,
                removed: 4,
            },
        ],
    }
}

fn base_row(base: &Base) -> LedgerRow<'_> {
    ledger(
        "shell",
        &base.arguments,
        Status::Completed,
        Some(0),
        Some(12),
        7,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: y",
        }),
        "first",
        Some("yes"),
        false,
    )
}

fn key_of(row: &LedgerRow<'_>) -> u64 {
    row.key("ledger_row")
}

#[test]
fn equal_inputs_share_a_key() {
    let base = base();
    assert_eq!(key_of(&base_row(&base)), key_of(&base_row(&base)));
}

#[test]
fn the_slot_is_in_the_key() {
    let base = base();
    let row = base_row(&base);
    assert_ne!(row.key("ledger_row"), row.key("ledger_row:shell"));
}

#[test]
fn the_name_is_in_the_key() {
    let base = base();
    let row = base_row(&base);
    let renamed = ledger(
        "read",
        &base.arguments,
        Status::Completed,
        Some(0),
        Some(12),
        7,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: y",
        }),
        "first",
        Some("yes"),
        false,
    );
    assert_ne!(key_of(&row), key_of(&renamed));
}

#[test]
fn a_nested_argument_value_is_in_the_key() {
    let base = base();
    let row = base_row(&base);
    let changed = serde_json::json!({"command": "ls", "nested": {"depth": 2}});
    let varied = ledger(
        "shell",
        &changed,
        Status::Completed,
        Some(0),
        Some(12),
        7,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: y",
        }),
        "first",
        Some("yes"),
        false,
    );
    assert_ne!(key_of(&row), key_of(&varied));
}

#[test]
fn argument_key_order_is_not_in_the_key() {
    let mut first = serde_json::Map::new();
    first.insert("a".to_owned(), serde_json::json!(1));
    first.insert("b".to_owned(), serde_json::json!(2));
    let mut second = serde_json::Map::new();
    second.insert("b".to_owned(), serde_json::json!(2));
    second.insert("a".to_owned(), serde_json::json!(1));
    let base = base();
    let row = base_row(&base);
    let one_value = serde_json::Value::Object(first);
    let two_value = serde_json::Value::Object(second);
    let one = ledger(
        "shell",
        &one_value,
        Status::Completed,
        Some(0),
        Some(12),
        7,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: y",
        }),
        "first",
        Some("yes"),
        false,
    );
    let two = ledger(
        "shell",
        &two_value,
        Status::Completed,
        Some(0),
        Some(12),
        7,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: y",
        }),
        "first",
        Some("yes"),
        false,
    );
    assert_ne!(key_of(&row), key_of(&one));
    assert_eq!(key_of(&one), key_of(&two));
}

#[test]
fn the_status_is_in_the_key() {
    let base = base();
    let row = base_row(&base);
    let running = ledger(
        "shell",
        &base.arguments,
        Status::Running,
        Some(0),
        Some(12),
        7,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: y",
        }),
        "first",
        Some("yes"),
        false,
    );
    assert_ne!(key_of(&row), key_of(&running));
}

#[test]
fn the_exit_is_in_the_key() {
    let base = base();
    let row = base_row(&base);
    let failed = ledger(
        "shell",
        &base.arguments,
        Status::Completed,
        Some(1),
        Some(12),
        7,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: y",
        }),
        "first",
        Some("yes"),
        false,
    );
    assert_ne!(key_of(&row), key_of(&failed));
}

#[test]
fn the_duration_is_in_the_key() {
    let base = base();
    let row = base_row(&base);
    let slower = ledger(
        "shell",
        &base.arguments,
        Status::Completed,
        Some(0),
        Some(13),
        7,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: y",
        }),
        "first",
        Some("yes"),
        false,
    );
    assert_ne!(key_of(&row), key_of(&slower));
}

#[test]
fn the_line_count_is_in_the_key() {
    let base = base();
    let row = base_row(&base);
    let longer = ledger(
        "shell",
        &base.arguments,
        Status::Completed,
        Some(0),
        Some(12),
        8,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: y",
        }),
        "first",
        Some("yes"),
        false,
    );
    assert_ne!(key_of(&row), key_of(&longer));
}

#[test]
fn a_change_is_in_the_key() {
    let base = base();
    let row = base_row(&base);
    for (index, varied) in [
        vec![
            FileChange {
                path: "c.rs".to_owned(),
                added: 1,
                removed: 2,
            },
            FileChange {
                path: "b.rs".to_owned(),
                added: 3,
                removed: 4,
            },
        ],
        vec![
            FileChange {
                path: "a.rs".to_owned(),
                added: 9,
                removed: 2,
            },
            FileChange {
                path: "b.rs".to_owned(),
                added: 3,
                removed: 4,
            },
        ],
        vec![
            FileChange {
                path: "a.rs".to_owned(),
                added: 1,
                removed: 9,
            },
            FileChange {
                path: "b.rs".to_owned(),
                added: 3,
                removed: 4,
            },
        ],
        vec![
            FileChange {
                path: "b.rs".to_owned(),
                added: 3,
                removed: 4,
            },
            FileChange {
                path: "a.rs".to_owned(),
                added: 1,
                removed: 2,
            },
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let other = ledger(
            "shell",
            &base.arguments,
            Status::Completed,
            Some(0),
            Some(12),
            7,
            &varied,
            Some(RowError {
                code: "nonzero_exit",
                message: "x: y",
            }),
            "first",
            Some("yes"),
            false,
        );
        assert_ne!(key_of(&row), key_of(&other), "case {index}");
    }
}

#[test]
fn the_error_is_in_the_key() {
    let base = base();
    let row = base_row(&base);
    let code = ledger(
        "shell",
        &base.arguments,
        Status::Completed,
        Some(0),
        Some(12),
        7,
        &base.changes,
        Some(RowError {
            code: "tool_error",
            message: "x: y",
        }),
        "first",
        Some("yes"),
        false,
    );
    let message = ledger(
        "shell",
        &base.arguments,
        Status::Completed,
        Some(0),
        Some(12),
        7,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: z",
        }),
        "first",
        Some("yes"),
        false,
    );
    let missing = ledger(
        "shell",
        &base.arguments,
        Status::Completed,
        Some(0),
        Some(12),
        7,
        &base.changes,
        None,
        "first",
        Some("yes"),
        false,
    );
    assert_ne!(key_of(&row), key_of(&code));
    assert_ne!(key_of(&row), key_of(&message));
    assert_ne!(key_of(&row), key_of(&missing));
}

#[test]
fn the_output_is_in_the_key() {
    let base = base();
    let row = base_row(&base);
    let other = ledger(
        "shell",
        &base.arguments,
        Status::Completed,
        Some(0),
        Some(12),
        7,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: y",
        }),
        "second",
        Some("yes"),
        false,
    );
    assert_ne!(key_of(&row), key_of(&other));
}

#[test]
fn the_answer_is_in_the_key() {
    let base = base();
    let row = base_row(&base);
    let other = ledger(
        "shell",
        &base.arguments,
        Status::Completed,
        Some(0),
        Some(12),
        7,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: y",
        }),
        "first",
        Some("no"),
        false,
    );
    let missing = ledger(
        "shell",
        &base.arguments,
        Status::Completed,
        Some(0),
        Some(12),
        7,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: y",
        }),
        "first",
        None,
        false,
    );
    assert_ne!(key_of(&row), key_of(&other));
    assert_ne!(key_of(&row), key_of(&missing));
}

#[test]
fn flat_is_in_the_key() {
    let base = base();
    let row = base_row(&base);
    let flat = ledger(
        "shell",
        &base.arguments,
        Status::Completed,
        Some(0),
        Some(12),
        7,
        &base.changes,
        Some(RowError {
            code: "nonzero_exit",
            message: "x: y",
        }),
        "first",
        Some("yes"),
        true,
    );
    assert_ne!(key_of(&row), key_of(&flat));
}

#[test]
fn an_empty_handle_fills_nothing() {
    assert!(Handle::default().get().is_none());
}

#[test]
fn a_set_renderer_is_what_get_returns() {
    use std::rc::Rc;
    let handle = Handle::create();
    assert!(handle.get().is_none());
    handle.set(Rc::new(Fake));
    assert!(handle.get().is_some());
    assert!(
        !handle
            .get()
            .expect("a renderer was set")
            .fills("ledger_row")
    );
}

#[test]
fn a_handle_on_another_thread_draws_built_in() {
    use std::rc::Rc;
    // The install lives on the thread that created it: resolving the
    // same handle elsewhere finds nothing, so the row falls back to
    // its built-in renderer without calling anything.
    let handle = Handle::create();
    handle.set(Rc::new(Fake));
    assert!(handle.get().is_some());
    // The install stays on this thread: resolving it elsewhere finds
    // nothing and never panics, so the row falls back to built-in.
    std::thread::spawn(move || assert!(handle.get().is_none()))
        .join()
        .expect("resolving a handle never panics");
    assert!(handle.get().is_some());
}

/// What the shared slot fake answers a render with.
#[derive(Debug, Clone)]
pub(crate) enum SlotScript {
    /// These lines.
    Replace(Vec<Vec<RawSpan>>),
    /// This failure.
    Fail {
        /// The failure family.
        kind: FailureKind,
        /// The Lua error, or what the spans got wrong.
        message: String,
    },
    /// The built-in at the width less `trim`, with these lines first.
    Decorate {
        /// The badge lines drawn before the built-in's.
        badge: Vec<Vec<RawSpan>>,
        /// The badge's cells, kept from the built-in's width.
        trim: u16,
    },
}

impl Default for SlotScript {
    fn default() -> Self {
        Self::Replace(vec![vec![raw("X")]])
    }
}

/// A plain span of `text`.
pub(crate) fn raw(text: &str) -> RawSpan {
    RawSpan {
        text: text.to_owned(),
        ..RawSpan::default()
    }
}

/// A test renderer for named slots: what it fills, what it saw, and
/// what it answers. It records only owned strings, never the input.
#[derive(Debug, Default)]
pub(crate) struct SlotRenderer {
    /// The slots it fills.
    pub(crate) fills: RefCell<Vec<String>>,
    /// The session, slot and width of every render call, in order.
    pub(crate) calls: RefCell<Vec<(Option<String>, String, u16)>>,
    /// The width of every built-in call, in order.
    pub(crate) builtins: RefCell<Vec<u16>>,
    /// What it answers.
    pub(crate) script: RefCell<SlotScript>,
}

impl SlotRenderer {
    /// A renderer filling `slots`, answering `script`.
    pub(crate) fn filling(slots: &[&str], script: SlotScript) -> Self {
        Self {
            fills: RefCell::new(slots.iter().map(|slot| slot.to_string()).collect()),
            calls: RefCell::new(Vec::new()),
            builtins: RefCell::new(Vec::new()),
            script: RefCell::new(script),
        }
    }
}

impl Renderer for SlotRenderer {
    fn name(&self) -> &str {
        "slot-test"
    }

    fn fills(&self, slot: &str) -> bool {
        self.fills.borrow().iter().any(|filled| filled == slot)
    }

    fn render(
        &self,
        ctx: &SessionCtx,
        slot: &str,
        input: &Input<'_>,
        width: u16,
        builtin: &dyn Fn(&Input<'_>, u16) -> Vec<Vec<RawSpan>>,
    ) -> Decision {
        decide(self.name(), || -> Result<Vec<Vec<RawSpan>>, Failure> {
            self.calls.borrow_mut().push((
                ctx.session_id.clone().map(|id| id.0),
                slot.to_owned(),
                width,
            ));
            match self.script.borrow().clone() {
                SlotScript::Replace(lines) => Ok(lines),
                SlotScript::Fail { kind, message } => Err(Failure {
                    kind,
                    extension: self.name().to_owned(),
                    message,
                }),
                SlotScript::Decorate { badge, trim } => {
                    let at = width.saturating_sub(trim);
                    self.builtins.borrow_mut().push(at);
                    let mut lines = builtin(input, at);
                    lines.splice(0..0, badge);
                    Ok(lines)
                }
            }
        })
    }
}

/// A seam holding `fake` on page `index` with session `session`: flat
/// draws drop every style.
pub(crate) fn seam(
    fake: &std::rc::Rc<SlotRenderer>,
    session: &str,
    index: usize,
    flat: bool,
) -> super::Bound {
    use std::rc::Rc;
    let handle = Handle::create();
    handle.set(Rc::clone(fake) as Rc<dyn Renderer>);
    let ext = Ext {
        handle,
        cache: std::sync::Arc::new(super::cache::Cache::default()),
        flat,
    };
    ext.bind(
        super::cache::PageKey {
            session: contract::SessionId(session.into()),
            index,
        },
        SessionCtx {
            session_id: Some(contract::SessionId(session.into())),
            halves: None,
        },
    )
}

/// A prompt input holding `text` with no images.
pub(crate) fn prompt(text: &str) -> Prompt<'_> {
    Prompt {
        text,
        images: Vec::new(),
        flat: false,
    }
}

/// A reply input holding `text`.
pub(crate) fn reply(text: &str) -> ReplyText<'_> {
    ReplyText { text, flat: false }
}

/// A thinking-line input for an ended thought.
pub(crate) fn thought(text: &str) -> ThinkingLine<'_> {
    ThinkingLine {
        text,
        started: 1,
        ended: Some(2),
        open: false,
        flat: false,
    }
}

/// The cache key of a prompt holding `text` with `images`.
fn prompt_key(text: &str, images: &[PromptImage<'_>]) -> u64 {
    Prompt {
        text,
        images: images.to_vec(),
        flat: false,
    }
    .key("prompt")
}

/// A card input holding one reply entry: the key tests vary it field
/// by field.
fn card() -> TurnCard<'static> {
    use super::{
        CardPrompt, TurnAnswers, TurnAside, TurnBand, TurnBandState, TurnCall, TurnEntry,
        TurnFailed, TurnGroup, TurnSection, TurnSizes, TurnStreaming, TurnThought,
    };
    TurnCard {
        prompts: vec![CardPrompt {
            text: "go",
            images: vec![PromptImage {
                path: "a.png",
                width: 10,
                height: 8,
            }],
        }],
        entries: vec![
            TurnEntry::Reply { text: "ok" },
            TurnEntry::Steer {
                text: "wait",
                images: Vec::new(),
                ts: 1,
            },
            TurnEntry::Group(TurnGroup {
                first: 1,
                last: 2,
                running: true,
                open: false,
                cut: false,
                sections: vec![TurnSection {
                    step: 3,
                    thoughts: vec![TurnThought {
                        text: "plan",
                        started: 4,
                        ended: Some(5),
                        open: false,
                    }],
                    calls: vec![TurnCall {
                        name: "read",
                        arguments: &serde_json::Value::Null,
                        status: Status::Completed,
                        exit: None,
                        duration_ms: None,
                        lines: 1,
                        changes: &[],
                        error: None,
                        output: "done",
                        headline: "",
                        content: "done",
                        answer: None,
                        images: Vec::new(),
                        open: false,
                        asking: false,
                    }],
                    failed: vec![TurnFailed {
                        code: "rate_limited",
                        attempt: 1,
                    }],
                }],
                streaming: vec![TurnStreaming {
                    message: "m",
                    index: 0,
                    name: None,
                    text: "arg",
                }],
            }),
            TurnEntry::Aside(TurnAside::Warning { message: "down" }),
            TurnEntry::Band(TurnBand {
                trigger: "automatic",
                ts: 6,
                state: TurnBandState::Done,
                sizes: Some(TurnSizes {
                    before: 7,
                    after: None,
                }),
            }),
            TurnEntry::Answers(TurnAnswers::Given {
                parts: vec![super::TurnAnswerPart {
                    header: "Base",
                    text: "main",
                    skipped: false,
                }],
                note: None,
                ts: 8,
            }),
        ],
        started: 1,
        calls: 0,
        usage: contract::shapes::Usage {
            tokens: contract::shapes::Tokens {
                input: 0,
                cache_read: 0,
                cache_write: std::collections::BTreeMap::new(),
                output: 0,
            },
            cost: None,
            subscription_cost: 0.0,
        },
        ended: None,
        running: true,
        flat: false,
    }
}

fn card_key(card: &TurnCard<'_>) -> u64 {
    card.key("turn_card")
}

#[test]
fn the_prompt_slot_is_in_the_key() {
    let image = PromptImage {
        path: "a.png",
        width: 10,
        height: 8,
    };
    let one = prompt("go").key("prompt");
    assert_ne!(one, prompt("go").key("reply"));
    assert_ne!(one, prompt_key("go", &[image]));
    assert_ne!(
        one,
        Prompt {
            text: "go",
            images: Vec::new(),
            flat: true,
        }
        .key("prompt")
    );
}

#[test]
fn a_prompt_image_is_in_the_key() {
    let at = PromptImage {
        path: "a.png",
        width: 10,
        height: 8,
    };
    let renamed = PromptImage {
        path: "b.png",
        width: 10,
        height: 8,
    };
    let wider = PromptImage {
        path: "a.png",
        width: 11,
        height: 8,
    };
    let taller = PromptImage {
        path: "a.png",
        width: 10,
        height: 9,
    };
    let one = prompt_key("go", &[at]);
    assert_ne!(one, prompt_key("stop", &[at]));
    for (index, varied) in [
        [renamed].as_slice(),
        [wider].as_slice(),
        [taller].as_slice(),
    ]
    .into_iter()
    .enumerate()
    {
        assert_ne!(one, prompt_key("go", varied), "case {index}");
    }
    assert_ne!(one, prompt_key("go", &[at, at]));
}

#[test]
fn the_reply_is_in_the_key() {
    let one = reply("ok").key("reply");
    assert_ne!(one, reply("ok").key("prompt"));
    assert_ne!(one, reply("changed").key("reply"));
    assert_ne!(
        one,
        ReplyText {
            text: "ok",
            flat: true,
        }
        .key("reply")
    );
}

#[test]
fn the_thinking_line_is_in_the_key() {
    let one = thought("plan").key("thinking_line");
    assert_ne!(one, thought("plan").key("reply"));
    assert_ne!(one, thought("other").key("thinking_line"));
    for (index, varied) in [
        ThinkingLine {
            text: "plan",
            started: 2,
            ended: Some(2),
            open: false,
            flat: false,
        },
        ThinkingLine {
            text: "plan",
            started: 1,
            ended: None,
            open: false,
            flat: false,
        },
        ThinkingLine {
            text: "plan",
            started: 1,
            ended: Some(2),
            open: true,
            flat: false,
        },
        ThinkingLine {
            text: "plan",
            started: 1,
            ended: Some(2),
            open: false,
            flat: true,
        },
    ]
    .into_iter()
    .enumerate()
    {
        assert_ne!(one, varied.key("thinking_line"), "case {index}");
    }
}

#[test]
fn the_turn_card_slot_and_prompts_are_in_the_key() {
    let one = card_key(&card());
    assert_ne!(one, card().key("prompt"));
    let mut varied = card();
    varied.prompts[0].text = "stop";
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.prompts[0].images[0].path = "b.png";
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.prompts[0].images[0].width = 11;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.prompts[0].images[0].height = 9;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.prompts[0].images.clear();
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.started = 2;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.calls = 1;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.running = false;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.flat = true;
    assert_ne!(one, card_key(&varied));
}

#[test]
fn each_entry_kind_changes_the_key() {
    use super::{TurnAnswers, TurnAside, TurnBand, TurnBandState, TurnEntry, TurnGroup};
    let one = card_key(&card());
    let swaps = [
        TurnEntry::Reply { text: "other" },
        TurnEntry::Steer {
            text: "wait",
            images: Vec::new(),
            ts: 2,
        },
        TurnEntry::Group(TurnGroup {
            first: 9,
            last: 9,
            running: false,
            open: false,
            cut: false,
            sections: Vec::new(),
            streaming: Vec::new(),
        }),
        TurnEntry::Aside(TurnAside::Warning { message: "up" }),
        TurnEntry::Band(TurnBand {
            trigger: "other",
            ts: 6,
            state: TurnBandState::Writing,
            sizes: None,
        }),
        TurnEntry::Answers(TurnAnswers::Declined),
    ];
    for (index, swap) in swaps.into_iter().enumerate() {
        let mut varied = card();
        varied.entries[index] = swap;
        assert_ne!(one, card_key(&varied), "entry {index}");
    }
}

fn card_group<'a, 'x>(card: &'a mut TurnCard<'x>) -> &'a mut super::TurnGroup<'x>
where
    'x: 'a,
{
    use super::TurnEntry;
    let TurnEntry::Group(group) = &mut card.entries[2] else {
        panic!("the third entry stays a group");
    };
    group
}

fn card_band<'a, 'x>(card: &'a mut TurnCard<'x>) -> &'a mut super::TurnBand<'x>
where
    'x: 'a,
{
    use super::TurnEntry;
    let TurnEntry::Band(band) = &mut card.entries[4] else {
        panic!("the fifth entry stays a band");
    };
    band
}

#[test]
fn the_group_entry_is_in_the_key() {
    let one = card_key(&card());
    let mut varied = card();
    card_group(&mut varied).first = 10;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).last = 10;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).running = false;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).open = true;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).cut = true;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].step = 4;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].thoughts[0].text = "other";
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].thoughts[0].started = 9;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].thoughts[0].ended = None;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].thoughts[0].open = true;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].calls[0].name = "shell";
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].calls[0].output = "other";
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].calls[0].headline = "x: y";
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].calls[0].content = "other";
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].calls[0].answer = Some("yes");
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].calls[0].open = true;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].calls[0].asking = true;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].calls[0].exit = Some(1);
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].calls[0].duration_ms = Some(3);
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].calls[0].lines = 2;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].failed[0].code = "busy";
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).sections[0].failed[0].attempt = 2;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).streaming[0].text = "other";
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_group(&mut varied).streaming[0].name = Some("shell");
    assert_ne!(one, card_key(&varied));
}

#[test]
fn the_call_status_and_error_are_in_the_key() {
    use super::{RowError, TurnCall, TurnEntry};
    let one = card_key(&card());
    for (index, status) in [
        Status::Pending,
        Status::Running,
        Status::Failed,
        Status::Denied,
        Status::Cancelled,
        Status::Unknown,
    ]
    .into_iter()
    .enumerate()
    {
        let mut varied = card();
        let TurnEntry::Group(group) = &mut varied.entries[2] else {
            panic!("the third entry stays a group");
        };
        group.sections[0].calls[0].status = status;
        assert_ne!(one, card_key(&varied), "status {index}");
    }
    let mut varied = card();
    let TurnEntry::Group(group) = &mut varied.entries[2] else {
        panic!("the third entry stays a group");
    };
    group.sections[0].calls[0].error = Some(RowError {
        code: "nonzero_exit",
        message: "x: y",
    });
    let erred = card_key(&varied);
    assert_ne!(one, erred);
    let TurnEntry::Group(group) = &mut varied.entries[2] else {
        panic!("the third entry stays a group");
    };
    let TurnCall { error, .. } = &mut group.sections[0].calls[0];
    error.as_mut().expect("an error").code = "tool_error";
    assert_ne!(erred, card_key(&varied));
    let TurnEntry::Group(group) = &mut varied.entries[2] else {
        panic!("the third entry stays a group");
    };
    let TurnCall { error, .. } = &mut group.sections[0].calls[0];
    error.as_mut().expect("an error").message = "x: z";
    assert_ne!(erred, card_key(&varied));
}

#[test]
fn the_band_state_is_in_the_key() {
    use super::{TurnBandState, TurnSizes};
    let one = card_key(&card());
    let mut varied = card();
    card_band(&mut varied).trigger = "other";
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_band(&mut varied).ts = 9;
    assert_ne!(one, card_key(&varied));
    for (index, state) in [
        TurnBandState::Writing,
        TurnBandState::Unchanged { cancelled: false },
        TurnBandState::Unchanged { cancelled: true },
    ]
    .into_iter()
    .enumerate()
    {
        let mut varied = card();
        card_band(&mut varied).state = state;
        assert_ne!(one, card_key(&varied), "state {index}");
    }
    let mut varied = card();
    card_band(&mut varied).sizes = None;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    card_band(&mut varied).sizes = Some(TurnSizes {
        before: 7,
        after: Some(9),
    });
    assert_ne!(one, card_key(&varied));
}

#[test]
fn the_steer_aside_and_answers_are_in_the_key() {
    use super::{TurnAnswers, TurnAside, TurnEntry, TurnOrphan};
    let one = card_key(&card());
    let mut varied = card();
    let TurnEntry::Steer { text, .. } = &mut varied.entries[1] else {
        panic!("the second entry stays a steer");
    };
    *text = "other";
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    let TurnEntry::Steer { ts, .. } = &mut varied.entries[1] else {
        panic!("the second entry stays a steer");
    };
    *ts = 9;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.entries[3] = TurnEntry::Aside(TurnAside::Line {
        text: "joined".into(),
    });
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.entries[3] = TurnEntry::Aside(TurnAside::Orphans {
        jobs: vec![TurnOrphan {
            name: "job",
            message: "left",
        }],
        open: false,
    });
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.entries[3] = TurnEntry::Aside(TurnAside::Orphans {
        jobs: vec![TurnOrphan {
            name: "job",
            message: "left",
        }],
        open: true,
    });
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.entries[5] = TurnEntry::Answers(TurnAnswers::Declined);
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    let TurnEntry::Answers(TurnAnswers::Given { parts, note, ts }) = &mut varied.entries[5] else {
        panic!("the sixth entry stays given");
    };
    parts[0].text = "other";
    *note = Some("note");
    *ts = 9;
    assert_ne!(one, card_key(&varied));
}

#[test]
fn the_ending_and_usage_are_in_the_key() {
    use super::{TurnEnded, TurnError};
    let one = card_key(&card());
    let mut varied = card();
    varied.ended = Some(TurnEnded {
        outcome: "completed",
        ended_ts: 2,
        error: None,
    });
    let ended = card_key(&varied);
    assert_ne!(one, ended);
    for (index, outcome) in ["interrupted", "failed", "cut short"]
        .into_iter()
        .enumerate()
    {
        let mut varied = card();
        varied.ended = Some(TurnEnded {
            outcome,
            ended_ts: 2,
            error: None,
        });
        assert_ne!(ended, card_key(&varied), "outcome {index}");
    }
    let mut varied = card();
    varied.ended = Some(TurnEnded {
        outcome: "failed",
        ended_ts: 3,
        error: Some(TurnError {
            code: contract::ErrorCode::IoFailed,
            message: "boom",
        }),
    });
    let failed = card_key(&varied);
    assert_ne!(ended, failed);
    let mut varied = card();
    varied.ended = Some(TurnEnded {
        outcome: "failed",
        ended_ts: 3,
        error: Some(TurnError {
            code: contract::ErrorCode::ConnectionFailed,
            message: "boom",
        }),
    });
    assert_ne!(failed, card_key(&varied));
    let mut varied = card();
    varied.ended = Some(TurnEnded {
        outcome: "failed",
        ended_ts: 3,
        error: Some(TurnError {
            code: contract::ErrorCode::IoFailed,
            message: "other",
        }),
    });
    assert_ne!(failed, card_key(&varied));
    let mut varied = card();
    varied.usage.tokens.input = 1;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.usage.tokens.cache_read = 1;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.usage.tokens.output = 1;
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.usage.tokens.cache_write.insert("default".into(), 1);
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.usage.cost = Some(1.0);
    assert_ne!(one, card_key(&varied));
    let mut varied = card();
    varied.usage.subscription_cost = 1.0;
    assert_ne!(one, card_key(&varied));
}

/// A group line with no nested rows: the key tests flip each scalar
/// and add one nested row each.
fn group_line(running: bool) -> super::GroupLine<'static> {
    super::GroupLine {
        running,
        calls: Vec::new(),
        streaming: Vec::new(),
        thoughts: Vec::new(),
        failed: Vec::new(),
        first: 1,
        last: 2,
        open: false,
        flat: false,
    }
}

fn group_call(name: &'static str) -> super::TurnCall<'static> {
    super::TurnCall {
        name,
        arguments: &serde_json::Value::Null,
        status: Status::Completed,
        exit: None,
        duration_ms: None,
        lines: 0,
        changes: &[],
        error: None,
        output: "",
        headline: "",
        content: "",
        answer: None,
        images: Vec::new(),
        open: false,
        asking: false,
    }
}

#[test]
fn every_group_line_field_is_in_the_key() {
    use super::{TurnFailed, TurnStreaming, TurnThought};
    let one = group_line(false).key("group_line");
    assert_ne!(one, group_line(false).key("ledger_row"));
    assert_ne!(one, group_line(true).key("group_line"));
    for (index, varied) in [
        super::GroupLine {
            first: 2,
            ..group_line(false)
        },
        super::GroupLine {
            last: 3,
            ..group_line(false)
        },
        super::GroupLine {
            open: true,
            ..group_line(false)
        },
        super::GroupLine {
            flat: true,
            ..group_line(false)
        },
        super::GroupLine {
            calls: vec![group_call("read")],
            ..group_line(false)
        },
        super::GroupLine {
            streaming: vec![TurnStreaming {
                message: "m",
                index: 0,
                name: None,
                text: "arg",
            }],
            ..group_line(false)
        },
        super::GroupLine {
            thoughts: vec![TurnThought {
                text: "plan",
                started: 1,
                ended: Some(2),
                open: false,
            }],
            ..group_line(false)
        },
        super::GroupLine {
            failed: vec![TurnFailed {
                code: "rate_limited",
                attempt: 1,
            }],
            ..group_line(false)
        },
    ]
    .into_iter()
    .enumerate()
    {
        assert_ne!(one, varied.key("group_line"), "case {index}");
    }
    // One nested row's fields flip it too.
    let mut varied = group_line(false);
    varied.calls = vec![group_call("read"), group_call("shell")];
    assert_ne!(one, varied.key("group_line"));
    let mut varied = group_line(false);
    varied.streaming = vec![TurnStreaming {
        message: "m",
        index: 1,
        name: None,
        text: "arg",
    }];
    assert_ne!(one, varied.key("group_line"));
    let mut varied = group_line(false);
    varied.thoughts = vec![TurnThought {
        text: "plan",
        started: 1,
        ended: Some(2),
        open: true,
    }];
    assert_ne!(one, varied.key("group_line"));
    let mut varied = group_line(false);
    varied.failed = vec![TurnFailed {
        code: "rate_limited",
        attempt: 2,
    }];
    assert_ne!(one, varied.key("group_line"));
}

#[test]
fn every_steer_field_is_in_the_key() {
    use super::Steer;
    let one = Steer {
        text: "wait",
        images: Vec::new(),
        ts: 1,
        flat: false,
    }
    .key("steer");
    assert_ne!(
        one,
        Steer {
            text: "wait",
            images: Vec::new(),
            ts: 1,
            flat: false,
        }
        .key("reply")
    );
    for (index, varied) in [
        Steer {
            text: "stop",
            images: Vec::new(),
            ts: 1,
            flat: false,
        },
        Steer {
            text: "wait",
            images: vec![PromptImage {
                path: "b.png",
                width: 10,
                height: 8,
            }],
            ts: 1,
            flat: false,
        },
        Steer {
            text: "wait",
            images: Vec::new(),
            ts: 2,
            flat: false,
        },
        Steer {
            text: "wait",
            images: Vec::new(),
            ts: 1,
            flat: true,
        },
    ]
    .into_iter()
    .enumerate()
    {
        assert_ne!(one, varied.key("steer"), "case {index}");
    }
}

#[test]
fn every_answers_field_is_in_the_key() {
    use super::{Answers, TurnAnswerPart};
    let given = || Answers {
        outcome: "given",
        parts: vec![TurnAnswerPart {
            header: "Base",
            text: "main",
            skipped: false,
        }],
        note: Some("go"),
        ts: Some(1),
        flat: false,
    };
    let one = given().key("answers");
    assert_ne!(one, given().key("reply"));
    let declined = Answers {
        outcome: "declined",
        parts: Vec::new(),
        note: None,
        ts: None,
        flat: false,
    };
    assert_ne!(one, declined.key("answers"));
    for (index, varied) in [
        Answers {
            outcome: "other",
            ..given()
        },
        Answers {
            parts: vec![
                TurnAnswerPart {
                    header: "Base",
                    text: "main",
                    skipped: false,
                },
                TurnAnswerPart {
                    header: "Name",
                    text: "skipped",
                    skipped: true,
                },
            ],
            ..given()
        },
        Answers {
            parts: vec![TurnAnswerPart {
                header: "Changed",
                text: "main",
                skipped: false,
            }],
            ..given()
        },
        Answers {
            parts: vec![TurnAnswerPart {
                header: "Base",
                text: "changed",
                skipped: false,
            }],
            ..given()
        },
        Answers {
            parts: vec![TurnAnswerPart {
                header: "Base",
                text: "main",
                skipped: true,
            }],
            ..given()
        },
        Answers {
            note: None,
            ..given()
        },
        Answers {
            ts: None,
            ..given()
        },
        Answers {
            flat: true,
            ..given()
        },
    ]
    .into_iter()
    .enumerate()
    {
        assert_ne!(one, varied.key("answers"), "case {index}");
    }
}

#[test]
fn every_handoff_band_field_is_in_the_key() {
    use super::{HandoffBand, TurnBandState, TurnSizes};
    let band = || HandoffBand {
        trigger: "the model handed off",
        ts: 1,
        state: TurnBandState::Done,
        sizes: Some(TurnSizes {
            before: 1,
            after: Some(2),
        }),
        flat: false,
    };
    let one = band().key("handoff_band");
    assert_ne!(one, band().key("reply"));
    for (index, varied) in [
        HandoffBand {
            trigger: "automatic",
            ..band()
        },
        HandoffBand { ts: 2, ..band() },
        HandoffBand {
            state: TurnBandState::Writing,
            sizes: None,
            ..band()
        },
        HandoffBand {
            state: TurnBandState::Unchanged { cancelled: false },
            sizes: None,
            ..band()
        },
        HandoffBand {
            state: TurnBandState::Unchanged { cancelled: true },
            sizes: None,
            ..band()
        },
        HandoffBand {
            sizes: None,
            ..band()
        },
        HandoffBand {
            sizes: Some(TurnSizes {
                before: 2,
                after: Some(2),
            }),
            ..band()
        },
        HandoffBand {
            sizes: Some(TurnSizes {
                before: 1,
                after: None,
            }),
            ..band()
        },
        HandoffBand {
            flat: true,
            ..band()
        },
    ]
    .into_iter()
    .enumerate()
    {
        assert_ne!(one, varied.key("handoff_band"), "case {index}");
    }
}

#[test]
fn every_error_line_field_is_in_the_key() {
    use super::ErrorLine;
    // One leaked provider per case: the key only reads the fields.
    let failure = |name: &str, status: Option<u16>, message: &str| {
        &*Box::leak(Box::new(contract::shapes::ProviderFailure {
            name: name.to_owned(),
            status,
            message: message.to_owned(),
        }))
    };
    let line = || ErrorLine {
        code: contract::ErrorCode::IoFailed,
        message: "read failed",
        provider: Some(failure("fake", Some(401), "bad key")),
        flat: false,
    };
    let one = line().key("error_line");
    assert_ne!(one, line().key("reply"));
    for (index, varied) in [
        ErrorLine {
            code: contract::ErrorCode::AuthenticationFailed,
            ..line()
        },
        ErrorLine {
            message: "changed",
            ..line()
        },
        ErrorLine {
            provider: None,
            ..line()
        },
        ErrorLine {
            flat: true,
            ..line()
        },
    ]
    .into_iter()
    .enumerate()
    {
        assert_ne!(one, varied.key("error_line"), "case {index}");
    }
    // The provider's every field flips it.
    for (index, provider) in [
        failure("other", Some(401), "bad key"),
        failure("fake", Some(500), "bad key"),
        failure("fake", None, "bad key"),
        failure("fake", Some(401), "other"),
    ]
    .into_iter()
    .enumerate()
    {
        let varied = ErrorLine {
            provider: Some(provider),
            ..line()
        };
        assert_ne!(one, varied.key("error_line"), "provider case {index}");
    }
}

#[test]
fn every_notice_field_is_in_the_key() {
    use super::Notice;
    let one = Notice {
        code: "io_failed",
        text: "read failed",
        flat: false,
    }
    .key("notice");
    assert_ne!(
        one,
        Notice {
            code: "io_failed",
            text: "read failed",
            flat: false,
        }
        .key("reply")
    );
    for (index, varied) in [
        Notice {
            code: "hook_failed",
            text: "read failed",
            flat: false,
        },
        Notice {
            code: "io_failed",
            text: "changed",
            flat: false,
        },
        Notice {
            code: "io_failed",
            text: "read failed",
            flat: true,
        },
    ]
    .into_iter()
    .enumerate()
    {
        assert_ne!(one, varied.key("notice"), "case {index}");
    }
}
