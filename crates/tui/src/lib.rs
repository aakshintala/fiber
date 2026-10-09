//! The terminal: one session on screen through the hub (`docs/tui.md`).
//!
//! [`run`] sets the injected tty up, draws the first frame, then starts the
//! threads that feed one loop: terminal bytes, hub lines and resizes. The
//! loop's only wait is a channel receive with no timeout, so nothing runs
//! while nothing happens. A frame that needs a page of history it dropped
//! fetches it with `history` and waits for the answer on the same channel
//! before it draws.

mod app;
mod appearance;
mod approvals;
mod attention;
mod bindings;
mod bubble;
mod cells;
mod clipboard;
mod configure;
#[cfg(test)]
mod configure_fake;
mod editor;
mod event_loop;
mod files;
mod focus;
mod format;
mod highlight;
mod home;
mod input;
mod jigs;
mod keymap;
mod keys;
mod keyset;
mod layout;
mod link;
mod local_time;
mod logical;
mod login_view;
mod look;
mod markdown;
mod mouse;
mod offer;
mod open_at;
mod opener;
mod osc;
mod pages;
mod paste_image;
#[cfg(test)]
mod results_support;
mod retry;
mod rows;
mod rules_view;
mod screen;
mod settings_view;
mod shell;
mod slash;
mod sources;
mod stroke;
mod surface;
mod swapped;
mod term;
mod theme;
mod tools_view;
mod turn;
mod turn_text;
mod usage_view;
mod view;
mod window;

use std::io;
use std::os::unix::net::UnixStream;

use contract::{HubLine, SessionId};

use crate::link::Line;

pub use attention::Attention;

pub use configure::{
    Configure, ConfigureError, Layer, LoginKind, LoginTarget, Revoked, RuleRow, RulesScope,
    RulesSection, Saved, SettingRow, Shown, Stored, SwitchScope, ToolGroup, ToolLists,
    ToolSwitches, WriteScope,
};

pub use home::Launch;

pub use open_at::OpenAt;

pub use look::ThemeSetting;

pub use keyset::KeysSetup;

pub use jigs::{draw, hover_frames, measure_paging};

pub use event_loop::run;

/// Connects to the hub, starting one when none runs: the stream and the
/// `hub_hello` it spoke first. Called again for each reconnect.
pub type Connect = Box<dyn FnMut() -> io::Result<(UnixStream, HubLine)> + Send>;

/// Called once with the session id when `start` is accepted.
pub type OnAttach = Box<dyn Fn(&SessionId) + Send>;

/// Saves a dragged share to the global configuration: its key and its
/// percent, or why it could not be saved.
pub type Save = Box<dyn Fn(&str, f64) -> Result<(), String> + Send>;

/// One thing the loop wakes for.
pub(crate) enum Input {
    /// Terminal bytes, one read.
    Bytes(Vec<u8>),
    /// One line from the hub.
    Hub(Line),
    /// The hub connected: the stream to write commands on, and its
    /// `hub_hello`.
    Connected(UnixStream, HubLine),
    /// The hub could not be reached.
    ConnectFailed(String),
    /// The hub connection ended.
    Disconnected,
    /// The terminal was resized.
    Resize,
    /// A file search result for the `@` panel, tagged with the generation
    /// it searched for: matching paths, or why there are none (the listing
    /// failed). A result for a generation no longer current is dropped.
    Files {
        /// The generation searched for.
        generation: u64,
        /// The paths found, or the listing's error.
        result: Result<Vec<String>, String>,
    },
    /// The search's pause after `generation`'s keystroke passed: a scan
    /// for another generation is stale (`docs/tui.md`, "Search").
    FindDue(u64),
    /// A clipboard image read finished for `ticket`: the image's bytes in
    /// base64, or the notice saying why there is no image. A result for a
    /// ticket no longer running, or a draft that moved on, is dropped.
    Image {
        /// The read's ticket.
        ticket: u64,
        /// The image in base64, or the notice.
        result: Result<String, String>,
    },
}

/// Restores the terminal [`run`] set up: turns mouse reporting off, leaves
/// the alternate screen, shows the cursor and restores the saved terminal
/// modes. Idempotent, takes no
/// lock, and does nothing when [`run`] never set the terminal up. The panic
/// hook calls it first.
pub fn restore() {
    term::restore();
}
