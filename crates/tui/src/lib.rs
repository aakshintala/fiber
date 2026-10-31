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
mod catalogue;
mod cells;
mod changed_files_view;
mod clipboard;
mod completion_rows;
mod configure;
#[cfg(test)]
mod configure_fake;
mod context_view;
mod convert;
mod diff;
mod editor;
mod event_loop;
pub mod ext;
mod feedback;
pub mod files;
mod focus;
mod format;
mod graphics;
mod helper;
mod highlight;
mod home;
mod image;
mod import_report;
mod input;
mod install_worker;
mod jigs;
mod keymap;
mod keys;
mod keyset;
mod layout;
mod link;
mod local_time;
mod logical;
mod login_view;
mod login_worker;
mod logout_view;
mod look;
mod markdown;
mod mcp_view;
mod model_picker;
mod motion;
mod mouse;
mod offer;
mod open_at;
mod opener;
mod osc;
mod pages;
mod paging_session;
mod paste_image;
#[cfg(test)]
mod pty_watch;
mod quota_view;
mod rebind;
mod release_notes;
mod remote;
#[cfg(test)]
mod results_support;
mod retry;
mod reviewer_setup;
mod rows;
mod rules_view;
mod running_view;
mod scope_switches;
mod screen;
mod settings_view;
mod shell;
mod skills_view;
mod slash;
mod sources;
mod status_view;
mod stroke;
mod surface;
mod swapped;
mod term;
mod text;
mod theme;
mod tick;
mod timing;
mod tools_view;
mod tty_screen;
mod turn;
mod turn_text;
mod usage_view;
mod view;
mod viewer;
mod window;
#[doc(hidden)]
pub mod work;
mod working;

use std::io;
use std::os::unix::net::UnixStream;

use contract::{HubLine, SessionId};

use crate::link::Line;

pub use attention::Attention;

pub use catalogue::LoggedOutProvider;
pub use catalogue::{
    Catalogue, LeftOutGroup, LeftOutModel, ListAge, ModelEntry, Price, PriceTier, ReadModels,
    Refresh,
};
pub use configure::{
    BrowserLogin, Configure, ConfigureError, FileOutcome, FileSaved, HostGap, HostSaved, KeyEdit,
    Layer, LoginKind, LoginNeed, LoginShow, LoginTarget, LogoutRow, NotesFile, NotesSaved,
    NotesWrite, ProviderInstall, ReadOnlyEntry, ReviewerModels, ReviewerNotes, Revoked, RuleRow,
    RulesScope, RulesSection, Saved, SettingRow, Shown, SkillsDisabled, Stored, SwitchScope,
    ToolGroup, ToolLists, ToolSwitches, WriteScope,
};
pub use contract::feedback::{FeedbackDone, WriteFeedback};

pub use home::{Launch, LaunchLocation};

pub use open_at::OpenAt;

pub use look::ThemeSetting;

pub use keyset::{KeysSetup, canonical_key, check_action, person_bound_keys};

pub use layout::{DEFAULT_PANEL_SHARE, DEFAULT_RAIL_SHARE, session_columns};

pub use jigs::{OpenStages, draw, hover_frames, measure_open, measure_paging};
pub use paging_session::paging_session;

pub use event_loop::run;

pub use ext::{
    CallState, Failure, Input as RendererInput, KeyOutcome, LedgerResult, LedgerRow, RawSpan,
    Renderer, RowError,
};

/// Connects to the hub, starting one when none runs: the stream and the
/// `hub_hello` it spoke first. Called again for each reconnect.
pub type Connect = Box<dyn FnMut() -> io::Result<(UnixStream, HubLine)> + Send>;

/// Called once with the session id when `start` is accepted.
pub type OnAttach = Box<dyn Fn(&SessionId) + Send>;

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
    /// A command write failed after its Loop boundary settled; the
    /// unsent commands are applied and settled as this follow-up input.
    WriteFailed(Vec<String>),
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
    /// A viewer worker finished opening image `id`: nothing on success,
    /// or why the open failed. A completion from an earlier session is
    /// dropped (`docs/tui.md`, "Images").
    Viewed {
        /// The image's id, so a failed open is not asked for again.
        id: u32,
        /// The file's name.
        name: String,
        /// The session generation that asked.
        generation: u64,
        /// Nothing, or why the open failed.
        result: Result<(), String>,
    },
    /// A conversion worker finished for `id`: the converted PNG in
    /// base64, already encoded on the worker, or why the image failed.
    /// A completion from an earlier session frees the one worker but
    /// draws nowhere (`docs/tui.md`, "Images").
    Converted {
        /// The session generation that asked.
        generation: u64,
        /// The placeholder id the converted PNG transmits under.
        id: u32,
        /// The converted PNG in base64, or the failure.
        result: Result<String, String>,
    },
    /// A model-list read answered: the installed models, or why they
    /// could not be read. An answer for a closed loop is dropped.
    Models(Result<Catalogue, String>),
    /// A feedback report finished writing: the report's path with its
    /// issue link, or why the report could not be written.
    Feedback(Result<FeedbackDone, String>),
    /// A browser login's progress or end for `ticket`: an event for a
    /// ticket no longer waiting changes nothing and opens nothing.
    Login {
        /// The waiting view's ticket.
        ticket: login_worker::LoginTicket,
        /// The login's progress or its end.
        step: login_worker::LoginStep,
    },
    /// A provider install from the model picker finished: the provider's
    /// name as the catalogue listed it, and the install's outcome or its
    /// error. A result for a provider not installing changes nothing.
    Install {
        /// The provider chosen.
        provider: String,
        /// What the install put in place, or why it failed.
        result: Result<ProviderInstall, ConfigureError>,
    },
    /// A hooks names query for the install offer answered for `ticket`:
    /// the sorted blocking names, or why they could not be read. A
    /// result for a ticket no longer waiting changes nothing.
    HooksOfferNames {
        /// The offer waiting on the names.
        ticket: u64,
        /// The blocking hook names, or the query's error.
        result: Result<Vec<String>, ConfigureError>,
    },
    /// A hooks install for the install offer finished for `ticket`:
    /// nothing, or why it failed. A result for a ticket no longer
    /// waiting changes nothing.
    HooksInstalled {
        /// The offer waiting on the install.
        ticket: u64,
        /// Nothing, or the install's error.
        result: Result<(), ConfigureError>,
    },
    /// The working line's timer passed: the frame the tick armed drew,
    /// and the next moving frame arms it again (`docs/tui.md`, "The
    /// working line").
    Tick,
    Signal(std::ffi::c_int),
    /// The Kitty writer thread's tty write failed: every pending image
    /// is dropped and nothing more is written, and the loop takes the
    /// terminal-failure path (`docs/tui.md`, "Images").
    KittyFailed,
}

/// Restores the terminal [`run`] set up: turns mouse reporting off, leaves
/// the alternate screen, shows the cursor and restores the saved terminal
/// modes. Idempotent, takes no
/// lock, and does nothing when [`run`] never set the terminal up. The panic
/// hook calls it first.
pub fn restore() {
    term::restore();
}
