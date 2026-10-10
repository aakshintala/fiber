//! The person's bindings in the app: loading `keys` at startup and
//! resolving every stroke through them (`docs/tui.md`, "Bindings").

use std::time::Instant;

use super::{App, Effect};
use crate::keys::{Event, default_event};
use crate::keyset::{Context, KeysSetup, Keyset, Resolved, load};
use crate::rebind::{KeysScreen, Outcome};
use crate::stroke::{Code, Mods, Stroke};
use crate::swapped::{Frame, Spot};

/// The plain Esc stroke: with the notice overlay open over the screen it
/// closes the overlay, ahead of the screen.
fn esc() -> Stroke {
    Stroke {
        code: Code::Esc,
        mods: Mods::NONE,
    }
}

/// The Ctrl+C stroke: through the bindings in every screen mode, because
/// the second Ctrl+C always quits (`docs/tui.md`, "Input and focus").
fn ctrl_c() -> Stroke {
    Stroke {
        code: Code::Char('c'),
        mods: Mods::CTRL,
    }
}

/// The app's effective bindings, and the `/keys` screen while it is open.
#[derive(Default)]
pub(super) struct Keyboard {
    keys: Keyset,
    screen: Option<KeysScreen>,
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

    /// Handles one stroke at `now`, read from the injected clock: the
    /// `/keys` screen first while it is open, else through the effective
    /// bindings in the context on screen. A stroke ends a finished
    /// attention title (`docs/tui.md`, "Getting the person's attention").
    pub(crate) fn on_press(&mut self, stroke: Stroke, now: Instant) -> Effect {
        self.attention_seen();
        if self.keys_screen_open() && !self.quit_open() {
            if self.notice_overlay().is_none() {
                let height = self.keys_height();
                let outcome = match self.keyboard.screen.as_mut() {
                    Some(screen) => screen.press(&stroke, &self.keyboard.keys, height),
                    None => Outcome::Nothing,
                };
                let effect = match outcome {
                    Outcome::Nothing => Effect::None,
                    Outcome::Close => {
                        self.keyboard.screen = None;
                        Effect::None
                    }
                    Outcome::Apply(next) => self.apply_keys(next),
                    // Ctrl+C goes through the bindings below, clearing
                    // then quitting as on any screen.
                    Outcome::Pass => return self.bindings_press(stroke, now),
                };
                self.edited();
                self.settle();
                return effect;
            }
            // The notice overlay hides the screen: Esc closes the
            // overlay, Ctrl+C goes through the bindings, and every other
            // stroke reaches neither the screen nor the draft.
            if stroke == esc() {
                self.notices.close();
                self.edited();
                self.settle();
                return Effect::None;
            }
            if stroke != ctrl_c() {
                return Effect::None;
            }
        }
        self.bindings_press(stroke, now)
    }

    /// One stroke through the effective bindings in the context on screen.
    /// Home owns plain arrows on a chip or a list stop: they dispatch
    /// as its own keys ahead of the bindings, so a rebound `recall_prompt`
    /// or `focus_next_prev` leaves them to home.
    fn bindings_press(&mut self, stroke: Stroke, now: Instant) -> Effect {
        if self.home_owns_arrows()
            && stroke.mods == Mods::NONE
            && matches!(
                stroke.code,
                Code::Up | Code::Down | Code::Left | Code::Right
            )
        {
            match default_event(&stroke) {
                Some(Event::Key(key)) => return self.on_key(key, now),
                Some(Event::Edit(edit)) => return self.on_edit(edit),
                Some(Event::Stroke(_) | Event::Mouse(_) | Event::Reply(_)) | None => {
                    return Effect::None;
                }
            }
        }
        match self.keys().resolve(&stroke, self.key_context()) {
            Resolved::Key(key) => self.on_key(key, now),
            Resolved::Edit(edit) => self.on_edit(edit),
            Resolved::Action(id) => self.on_action(id),
            Resolved::Nothing => Effect::None,
        }
    }

    /// Opens the `/keys` screen: the draft cleared, and the key map, the
    /// model picker, a configuration view and a session view closed, as
    /// opening the model picker closes the other views. One swapped view
    /// shows at a time.
    pub(crate) fn open_keys(&mut self) -> Effect {
        self.draft.clear();
        self.close_keymap();
        self.model_picker.close();
        self.close_config_view();
        self.close_session_view();
        self.keyboard.screen = Some(KeysScreen::default());
        Effect::None
    }

    /// Whether the `/keys` screen is open.
    pub(crate) fn keys_screen_open(&self) -> bool {
        self.keyboard.screen.is_some()
    }

    /// The screen's frame at `height`; `None` while it is closed.
    pub(crate) fn keys_frame(&self, height: usize) -> Option<Frame> {
        self.keyboard
            .screen
            .as_ref()
            .map(|screen| screen.frame(&self.keyboard.keys, height))
    }

    /// A click on the screen's `spot`: the ✕ closes it, a row selects
    /// while browsing.
    pub(crate) fn keys_screen_click(&mut self, spot: Spot) -> Effect {
        let height = self.keys_height();
        let close = self
            .keyboard
            .screen
            .as_mut()
            .is_some_and(|screen| matches!(screen.click(spot, height), Outcome::Close));
        if close {
            self.keyboard.screen = None;
        }
        self.edited();
        self.settle();
        Effect::None
    }

    /// The screen's height in rows: the attached conversation's, or the
    /// screen's on home, as the model picker computes its own.
    fn keys_height(&self) -> usize {
        if self.on_home() {
            usize::from(self.screen.height())
        } else {
            self.conversation_height()
        }
    }

    /// Saves one confirmed screen change: the edits against the old
    /// keyset, in one call. No edits saves nothing; a refused save keeps
    /// the old keyset on screen with the failure's message. With no seam
    /// the change applies for this run only.
    fn apply_keys(&mut self, next: Keyset) -> Effect {
        let edits = next.edits(&self.keyboard.keys);
        if edits.is_empty() {
            return Effect::None;
        }
        let saved = match self.configure_seam() {
            Some(seam) => seam.save_keys(&edits).map_err(|error| error.message),
            None => Ok(()),
        };
        match saved {
            Ok(()) => {
                self.keyboard.keys = next;
            }
            Err(message) => {
                if let Some(screen) = self.keyboard.screen.as_mut() {
                    screen.failed(message);
                }
            }
        }
        Effect::None
    }

    /// The context a stroke arrives in, front to back: the quit question,
    /// an overlay (a configuration or session view, a home modal, the key
    /// map above the model picker, the approval panel, an offer, the
    /// search bar), the model picker, a focused chip as the input box,
    /// the focused conversation, a completion panel, a steering
    /// selection, else the input box. The notice overlay is no context:
    /// it takes only Esc, which is `close_or_interrupt`'s key everywhere.
    /// Home's arrows read it through `home_owns_arrows`.
    pub(super) fn key_context(&self) -> Context {
        if self.quit_open()
            || self.config_view_open()
            || self.session_view_open()
            || self.home_modal()
            || self.keymap_top().is_some()
            || self.panel().is_some()
            || self.offer_open()
            || self.search_panel().is_some()
        {
            Context::Overlay
        } else if self.model_picker_open() {
            Context::Picker
        } else if self.find_open() {
            Context::Search
        } else if self.focused_chip().is_some() {
            Context::Input
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
    /// the cursor is in the input box. `session_only` chooses in the open
    /// model picker for this session only. Anything else does nothing.
    fn on_action(&mut self, id: &'static str) -> Effect {
        if id == "session_only" {
            return self.model_picker_session_only();
        }
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
#[path = "keyboard_screen_tests.rs"]
mod screen_tests;

#[cfg(test)]
#[path = "keyboard_tests.rs"]
mod tests;
