//! Tests for the seam types: the input key and the renderer handle.

use super::{Decision, FailureKind, SessionCtx, SlotSelection};
use super::{Failure, Handle, Input, LedgerRow, RawSpan, Renderer, RowError, Status};
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
    pub(crate) calls: RefCell<Vec<String>>,
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
        self.calls.borrow_mut().push(ctx.session_id.0.clone());
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
