//! The renderer seam one slot draws through (`docs/tui.md`, "Extension seams").
//!
//! A slot's renderer draws what follows the slot's own chrome with the
//! width less that chrome, and returns lines of spans. `tui` owns the
//! trait and the span rules; `main` adapts the extensions VM to it, so
//! `tui` depends on no crate that holds the VM (`docs/architecture.md`,
//! "The call rules"). `tui` tests use a fake renderer.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

pub(crate) mod cache;

use cache::Cache;
pub(crate) mod input;

pub use input::{
    AnswerDisposition, Answers, CallState, CardPrompt, ErrorLine, GroupLine, HandoffBand, Input,
    LedgerResult, LedgerRow, Notice, Prompt, PromptImage, ReplyText, RowError, Steer, ThinkingLine,
    TurnAnswerPart, TurnAnswers, TurnAside, TurnBand, TurnBandState, TurnCall, TurnCallResult,
    TurnCard, TurnCardState, TurnEnded, TurnEntry, TurnError, TurnFailed, TurnGroup,
    TurnGroupState, TurnOrphan, TurnSection, TurnSizes, TurnStreaming, TurnThought,
};
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
    /// The session receiving the callback; none on home, which has no
    /// session, so no previous session's identity is reused and no
    /// empty or synthetic id is sent.
    pub session_id: Option<contract::SessionId>,
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

impl Bound {
    /// The bound renderer's `notice` lines for one notice: `None`
    /// without an installed renderer, without a winner, or when the
    /// renderer misses or fails, so the row falls back to built-in.
    /// `extension_failed` always draws built-in. A renderer returning
    /// no lines hides the notice instead; rows, counts and the overlay
    /// share the one cached lookup, so each distinct notice calls the
    /// renderer once (`docs/tui.md`, "How a TUI extension runs").
    pub(crate) fn notice_lines(
        &self,
        code: &str,
        text: &str,
        width: u16,
    ) -> Option<Arc<Vec<spans::FittedLine>>> {
        if code == "extension_failed" {
            return None;
        }
        let renderer = self.handle.get()?;
        let selected = renderer.fills_for(&self.ctx, "notice");
        for queued in renderer.take_pending() {
            self.cache.queue_notice(queued);
        }
        let SlotSelection::Draw { extension } = selected else {
            return None;
        };
        let input = Input::Notice(Notice {
            code,
            text,
            flat: self.flat,
        });
        let fitted = self.cache.lines(
            self.page.clone(),
            "notice",
            &extension,
            renderer.half(&self.ctx, &extension),
            &input,
            width,
            || {
                renderer.render(&self.ctx, "notice", &input, width, &|input, at| {
                    let Input::Notice(notice) = input else {
                        return Vec::new();
                    };
                    crate::view::notice_rows::builtin_spans(notice.code, notice.text, at as usize)
                })
            },
        );
        for queued in renderer.take_pending() {
            self.cache.queue_notice(queued);
        }
        fitted
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
