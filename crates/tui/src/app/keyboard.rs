//! The person's bindings in the app: loading `keys` at startup and
//! resolving every stroke through them (`docs/tui.md`, "Bindings").

use std::time::Instant;

use super::{App, Effect};
use crate::keyset::{Context, KeysSetup, Keyset, Resolved, load};
use crate::stroke::Stroke;

/// The app's effective bindings.
#[derive(Default)]
pub(super) struct Keyboard {
    keys: Keyset,
}

impl App {
    /// Loads the person's `keys`, pushing one notice per invalid or
    /// clashing entry. Runs before `set_home`, so the notices land in the
    /// notices the first frame draws.
    pub(crate) fn set_keys(&mut self, setup: KeysSetup) {
        let (keys, notices) = load(&setup.user);
        self.keyboard.keys = keys;
        for notice in notices {
            self.notices.push(notice);
        }
    }

    /// The effective bindings, what the key map shows.
    pub(crate) fn keys(&self) -> &Keyset {
        &self.keyboard.keys
    }

    /// Handles one stroke at `now`, read from the injected clock: through
    /// the effective bindings in the context on screen. A stroke ends a
    /// finished attention title (`docs/tui.md`, "Getting the person's
    /// attention").
    pub(crate) fn on_press(&mut self, stroke: Stroke, now: Instant) -> Effect {
        self.attention_seen();
        match self.keys().resolve(&stroke, self.key_context()) {
            Resolved::Key(key) => self.on_key(key, now),
            Resolved::Edit(edit) => self.on_edit(edit),
            Resolved::Action(id) => self.on_action(id),
            Resolved::Nothing => Effect::None,
        }
    }

    /// The context a stroke arrives in, first match wins, mirroring
    /// `route_key`'s handler order: an overlay, the search bar, the
    /// focused conversation, a completion panel, a steering selection,
    /// else the input box. The notice overlay is no context: it takes
    /// only Esc, which is `close_or_interrupt`'s key everywhere.
    fn key_context(&self) -> Context {
        if self.quit_open()
            || self.config_view_open()
            || self.home_modal()
            || self.keymap_top().is_some()
            || self.panel().is_some()
            || self.offer_open()
            || self.search_panel().is_some()
        {
            Context::Overlay
        } else if self.find_open() {
            Context::Search
        } else if self.focus.is_some() {
            Context::Conversation
        } else if self.completions().is_some() {
            Context::Overlay
        } else if self.steering.is_selected() {
            Context::Steering
        } else {
            Context::Input
        }
    }

    /// Runs an action: `new_session` and `go_home` leave for home the way
    /// `/new` does, dropping focus and any steering selection first, so
    /// the cursor is in the input box. Anything else does nothing.
    fn on_action(&mut self, id: &'static str) -> Effect {
        if !matches!(id, "new_session" | "go_home") {
            return Effect::None;
        }
        self.history.cancel();
        self.armed_at = None;
        self.copied = false;
        self.focus = None;
        self.steering.clear(&mut self.draft);
        let effect = self.leave();
        self.edited();
        self.settle();
        effect
    }
}

#[cfg(test)]
#[path = "keyboard_tests.rs"]
mod tests;
