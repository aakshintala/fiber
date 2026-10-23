//! The Lua VM of one TUI extension (`docs/tui.md`, "How a TUI extension
//! runs").

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use contract::clock::Clock;

/// The slot names that stand alone (`docs/tui.md`, "The slots").
pub(crate) const SLOT_NAMES: [&str; 20] = [
    "layout",
    "home",
    "turn_card",
    "prompt",
    "reply",
    "group_line",
    "ledger_row",
    "thinking_line",
    "steer",
    "answers",
    "handoff_band",
    "error_line",
    "notice",
    "working_line",
    "steering_queue",
    "input_box",
    "status_rows",
    "approval",
    "question_form",
    "repository_offer",
];

/// The slot names that take a suffix after the colon.
pub(crate) const SLOT_PREFIXES: [&str; 4] = ["ledger_row:", "card:", "view:", "overlay:"];

/// Whether `name` is a slot name (`docs/tui.md`, "The slots").
fn slot_name_ok(name: &str) -> bool {
    SLOT_NAMES.contains(&name)
        || SLOT_PREFIXES.iter().any(|prefix| {
            name.strip_prefix(prefix)
                .is_some_and(|rest| !rest.is_empty())
        })
}

/// One run of text in a line a renderer returns (`docs/tui.md`, "What a
/// renderer returns").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Span {
    /// The text.
    pub text: String,
    /// A theme colour role for the foreground.
    pub fg: Option<String>,
    /// A theme colour role for the background.
    pub bg: Option<String>,
    /// Bold.
    pub bold: bool,
    /// Dim.
    pub dim: bool,
    /// Italic.
    pub italic: bool,
    /// Underlined.
    pub underline: bool,
    /// The URL the span opens on click.
    pub link: Option<String>,
    /// The id of the span's click target.
    pub click: Option<String>,
}

/// Why a TUI extension's VM did not answer.
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    /// The script or a callback raised a Lua error.
    #[error("{0}")]
    Lua(String),
    /// A callback passed its declared timeout.
    #[error("the callback passed its timeout")]
    Timeout,
    /// An earlier timeout switched the VM off.
    #[error("the extension is switched off until /reload")]
    Off,
    /// The extension registered no such slot.
    #[error("the extension registers no slot `{0}`")]
    NoSlot(String),
    /// A renderer returned the wrong shape, or was given one.
    #[error("wrong shape: {0}")]
    Shape(String),
    /// `fiber.slot` refused a registration.
    #[error("{0}")]
    Registration(String),
}

/// One extension's Lua VM, owned by the thread that calls it.
pub struct TuiVm {
    name: String,
    slots: BTreeMap<String, Duration>,
}

impl std::fmt::Debug for TuiVm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiVm")
            .field("name", &self.name)
            .field("slots", &self.slots.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl TuiVm {
    /// Runs `source`, the extension `name`'s entry script, and returns the VM
    /// holding the slots it registered.
    pub fn load(_name: &str, _source: &str, _clock: Arc<dyn Clock>) -> Result<Self, TuiError> {
        Err(TuiError::Lua(slot_name_ok("").to_string()))
    }

    /// Each registered slot and its declared timeout, by name.
    pub fn slots(&self) -> Vec<(String, Duration)> {
        self.slots.iter().map(|(n, t)| (n.clone(), *t)).collect()
    }

    /// Calls the renderer of `slot` for `session_id`.
    pub fn call(
        &mut self,
        _slot: &str,
        _session_id: &str,
        _input: &serde_json::Value,
        _width: u16,
        _builtin: &dyn Fn(&serde_json::Value, u16) -> Vec<Vec<Span>>,
    ) -> Result<Vec<Vec<Span>>, TuiError> {
        Err(TuiError::Lua(String::new()))
    }
}
