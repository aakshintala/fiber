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

use contract::events::FileChange;

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
}

/// A ledger row's call: the call's name, arguments, status, exit code,
/// duration, line count, `changes` and error, the first line of the
/// result's text, a form answer's suffix, and whether the screen reader
/// drops every style.
#[derive(Debug, Clone)]
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
