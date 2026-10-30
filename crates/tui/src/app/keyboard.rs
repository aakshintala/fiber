//! The person's bindings in the app: loading `keys` at startup and
//! resolving every stroke through them (`docs/tui.md`, "Bindings").

use std::time::Instant;

use std::rc::Rc;

use super::{App, Effect, Surface};
use crate::ext::{KeyOutcome, SessionCtx};
use crate::keymap::{KeyMap, KeyPress};
use crate::keys::{Edit, Event, Key, default_event};
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

/// Whether `stroke` is bare typing: a single character with no modifier
/// but Shift, or the space bar with no modifier but Shift
/// (`docs/tui.md`, "Bindings"). Typing reaches the input box, so a `run`
/// binding never sees it in the input context.
fn is_bare_typing(stroke: &Stroke) -> bool {
    match stroke.code {
        Code::Char(_) | Code::Space => stroke.mods == Mods::NONE || stroke.mods == Mods::SHIFT,
        Code::Enter
        | Code::Esc
        | Code::Tab
        | Code::Backspace
        | Code::Delete
        | Code::Insert
        | Code::Home
        | Code::End
        | Code::PageUp
        | Code::PageDown
        | Code::Up
        | Code::Down
        | Code::Left
        | Code::Right
        | Code::F(_) => false,
    }
}

/// The app's effective bindings, and the `/keys` screen while it is open.
#[derive(Default)]
pub(super) struct Keyboard {
    keys: Keyset,
    screen: Option<KeysScreen>,
    keymap: Option<KeyMap>,
}

impl Keyboard {
    /// The key map overlay, while it is open.
    pub(super) fn keymap(&self) -> Option<&KeyMap> {
        self.keymap.as_ref()
    }

    /// The key map overlay's top row, while it is open.
    pub(super) fn keymap_top(&self) -> Option<usize> {
        self.keymap.as_ref().map(KeyMap::top)
    }

    /// Closes the key map before a swapped view takes the conversation.
    pub(super) fn close_keymap(&mut self) {
        self.keymap = None;
    }

    /// Opens the key map overlay at its top.
    pub(super) fn open_keymap(&mut self) -> Effect {
        self.keymap = Some(KeyMap::default());
        Effect::None
    }

    /// A key while the key map is open: it takes every key but Ctrl+C,
    /// ⌥R and ⌥1–9, which arm quit and switch sessions past it. `None`
    /// while it is closed.
    pub(super) fn keymap_key(
        &mut self,
        key: &Key,
        keyset: &Keyset,
        width: u16,
        height: usize,
    ) -> Option<Effect> {
        let map = self.keymap.as_mut()?;
        // The frame docks full width, so the inner columns are the
        // conversation's less the frame's four blank ones.
        match map.press(key, keyset, width.saturating_sub(4), height) {
            KeyPress::Close => self.keymap = None,
            KeyPress::Through => return None,
            KeyPress::Handled => {}
        }
        Some(Effect::None)
    }

    /// An edit while the key map is open: Left and Right move between
    /// its tabs, every other edit does nothing behind it, and a paste
    /// never reaches the draft. `None` while it is closed.
    pub(super) fn edit_key(&mut self, edit: &Edit) -> Option<Effect> {
        let map = self.keymap.as_mut()?;
        match edit {
            Edit::Left => map.move_tab(-1),
            Edit::Right => map.move_tab(1),
            Edit::ShiftEnter
            | Edit::CtrlJ
            | Edit::WordLeft
            | Edit::WordRight
            | Edit::DeleteWord
            | Edit::DeleteToLineStart
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Delete
            | Edit::Paste(_) => {}
        }
        Some(Effect::None)
    }
}

impl App {
    /// Loads the person's `keys`, pushing one notice per invalid or
    /// clashing entry. Runs before `set_home`, so the notices land in the
    /// notices the first frame draws.
    pub(crate) fn set_keys(&mut self, setup: KeysSetup) {
        let (keys, notices) = load(&setup.user);
        self.keyboard.keys = keys;
        for notice in notices {
            self.notices.push("config_invalid", notice);
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
    #[cfg(test)]
    pub(crate) fn on_press(&mut self, stroke: Stroke, now: Instant) -> Effect {
        let (effect, settle) = self.press_route(stroke, now);
        if settle {
            self.settle();
        }
        effect
    }

    /// Handles one stroke without settling; the input boundary owns the layout pass.
    pub(crate) fn on_press_inner(&mut self, stroke: Stroke, now: Instant) -> Effect {
        self.press_route(stroke, now).0
    }

    fn press_route(&mut self, stroke: Stroke, now: Instant) -> (Effect, bool) {
        self.attention_seen();
        if self.top_surface() == Surface::KeysScreen {
            // The notice overlay hides the screen: Esc closes the
            // overlay, Ctrl+C goes through the bindings, and every other
            // stroke reaches neither the screen nor the draft. A `None`
            // falls through to the bindings below.
            let effect = if self.notice_overlay().is_none() {
                let height = self.keys_height();
                let outcome = match self.keyboard.screen.as_mut() {
                    Some(screen) => screen.press(&stroke, &self.keyboard.keys, height),
                    None => Outcome::Nothing,
                };
                match outcome {
                    Outcome::Nothing => Some(Effect::None),
                    Outcome::Close => {
                        self.keyboard.screen = None;
                        Some(Effect::None)
                    }
                    Outcome::Apply(next) => Some(self.apply_keys(next)),
                    // Ctrl+C goes through the bindings below, clearing
                    // then quitting as on any screen.
                    Outcome::Pass => None,
                }
            } else if stroke == esc() {
                self.notices.close();
                Some(Effect::None)
            } else if stroke == ctrl_c() {
                None
            } else {
                return (Effect::None, false);
            };
            if let Some(effect) = effect {
                self.edited();
                return (effect, true);
            }
        }
        // Every binding uses unsettled routes. The direct `on_press`
        // wrapper settles when requested; `on_press_inner` leaves the
        // layout pass to the Loop input boundary.
        let mut routed = false;
        let (effect, settle) = self.bindings_press_with_routes(stroke, now, &mut routed);
        (effect, settle || routed)
    }

    /// One stroke through the whole-screen extension bindings, ahead of
    /// every built-in early path (`docs/tui.md`, "Input and focus"): an
    /// eligible `run` binding first, then an action binding where the
    /// Bindings table lists it. Neither kind acts with no session on
    /// screen, on a job's live PTY view, or for a key the person set in
    /// the live keyset. Returns the dispatch with whether the Loop entry
    /// still settles: a handled `run` or action settles with its own
    /// handling, while drained adapter notices settle the fallthrough
    /// the built-in paths still run.
    fn extension_press(
        &mut self,
        stroke: &Stroke,
        now: Instant,
        routed: &mut bool,
    ) -> (Option<(Effect, bool)>, bool) {
        // Neither kind of binding acts with no session on screen.
        let Some(session) = self.session().cloned() else {
            return (None, false);
        };
        // A job's live PTY view owns every keystroke but its built-ins.
        if self.item_job_target().is_some() {
            return (None, false);
        }
        let Some(renderer) = self.handle.get() else {
            return (None, false);
        };
        let Some(canonical) = crate::canonical_key(&stroke.name()) else {
            return (None, false);
        };
        let halves = self.pages().halves_for(&session);
        let ctx = SessionCtx {
            session_id: Some(session),
            halves,
        };
        let context = self.key_context();
        let person_bound = self.keys().person_bound_any(stroke);
        // A `run` binding acts everywhere but its run-only exclusions: no
        // Overlay, Picker or Search surface, and no bare typing in the
        // input box, where typing reaches the input box instead.
        let run_eligible = !matches!(
            context,
            Context::Overlay | Context::Picker | Context::Search
        ) && !(context == Context::Input && is_bare_typing(stroke));
        if run_eligible && renderer.run_key(&ctx, &canonical, person_bound) == KeyOutcome::Handled {
            self.drain_key_notices(&renderer);
            return (Some((Effect::None, true)), false);
        }
        // An action binding acts where the Bindings table lists it for
        // the current context.
        if let Some((id, variant)) = renderer.key_action(&ctx, &canonical, person_bound)
            && crate::bindings::BINDINGS
                .iter()
                .any(|binding| binding.id == id && binding.contexts.contains(context))
            && let Some(event) = Keyset::action_event(&id, variant.as_deref())
        {
            self.drain_key_notices(&renderer);
            return (
                Some(match event {
                    Resolved::Key(key) => (
                        {
                            *routed = true;
                            self.on_key_inner(key, now)
                        },
                        false,
                    ),
                    Resolved::Edit(edit) => (
                        {
                            *routed = true;
                            self.on_edit_inner(edit)
                        },
                        false,
                    ),
                    Resolved::Action(id) => (self.on_action(id), true),
                    Resolved::Nothing => (Effect::None, false),
                }),
                false,
            );
        }
        // Nothing handled the stroke: the adapter's queued notices settle
        // the fallthrough, and the built-in paths below still run.
        let mut noticed = false;
        for (code, text) in renderer.take_pending() {
            self.push_notice(&code, text);
            noticed = true;
        }
        (None, noticed)
    }

    /// Drains the renderer's queued key notices onto the screen.
    fn drain_key_notices(&mut self, renderer: &Rc<dyn crate::ext::Renderer>) {
        for (code, text) in renderer.take_pending() {
            self.push_notice(&code, text);
        }
    }

    /// One stroke through the whole-screen extension bindings, then the
    /// built-in paths and the effective bindings in the context on
    /// screen. Returns the effect with whether the Loop entry still
    /// settles: this test entry settles key/edit dispatch itself, while
    /// actions, handled extension bindings and drained notices leave the
    /// settle to the caller. A stroke nothing handles settles nothing.
    #[cfg(test)]
    fn bindings_press(&mut self, stroke: Stroke, now: Instant) -> (Effect, bool) {
        let mut routed = false;
        let dispatch = self.bindings_press_with_routes(stroke, now, &mut routed);
        if routed {
            self.settle();
        }
        dispatch
    }

    fn bindings_press_with_routes(
        &mut self,
        stroke: Stroke,
        now: Instant,
        routed: &mut bool,
    ) -> (Effect, bool) {
        let (dispatch, noticed) = self.extension_press(&stroke, now, routed);
        if let Some(dispatch) = dispatch {
            return dispatch;
        }
        let (effect, settle) = self.builtin_press(stroke, now, routed);
        (effect, settle || noticed)
    }

    /// One stroke through the built-in early paths and the effective
    /// bindings in the context on screen. Home owns plain arrows on a
    /// chip or a list stop: they dispatch as its own keys ahead of the
    /// bindings, so a rebound `recall_prompt` or `focus_next_prev`
    /// leaves them to home.
    fn builtin_press(&mut self, stroke: Stroke, now: Instant, routed: &mut bool) -> (Effect, bool) {
        if let Some(effect) = self.rail_stroke(&stroke, now) {
            *routed = true;
            return (effect, false);
        }
        // Unshifted Home edits the input editor's line, but only where
        // typing would reach it: a person's own binding for the stroke
        // wins, and every other context keeps its dispatch
        // (`docs/tui.md`, "Selection and copy"). `on_edit` still gives
        // focus, the find bar and every overlay above the draft the
        // first say, as it does for typed characters. End keeps its
        // `Key::End` identity everywhere and edits the line in the input
        // box's own key arm, so its recall-drop and jump-to-end survive.
        if stroke.code == Code::Home
            && let Some(edit) = crate::keys::home_end_edit(&stroke)
        {
            let context = self.key_context();
            if matches!(
                context,
                Context::Input | Context::Steering | Context::Conversation
            ) && !self.keys().person_bound(&stroke, context)
            {
                return (
                    {
                        *routed = true;
                        self.on_edit_inner(edit)
                    },
                    false,
                );
            }
        }
        if self.home_owns_arrows()
            && stroke.mods == Mods::NONE
            && matches!(
                stroke.code,
                Code::Up | Code::Down | Code::Left | Code::Right
            )
        {
            match default_event(&stroke) {
                Some(Event::Key(key)) => {
                    return (
                        {
                            *routed = true;
                            self.on_key_inner(key, now)
                        },
                        false,
                    );
                }
                Some(Event::Edit(edit)) => {
                    return (
                        {
                            *routed = true;
                            self.on_edit_inner(edit)
                        },
                        false,
                    );
                }
                Some(Event::Stroke(_) | Event::Mouse(_) | Event::Reply(_) | Event::Focus(_))
                | None => {
                    return (Effect::None, false);
                }
            }
        }
        match self.keys().resolve(&stroke, self.key_context()) {
            Resolved::Key(key) => (
                {
                    *routed = true;
                    self.on_key_inner(key, now)
                },
                false,
            ),
            Resolved::Edit(edit) => (
                {
                    *routed = true;
                    self.on_edit_inner(edit)
                },
                false,
            ),
            Resolved::Action(id) => (self.on_action(id), true),
            Resolved::Nothing => (Effect::None, false),
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
    /// while browsing. The click entry settles after the dispatch.
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
        Effect::None
    }

    /// Shifts the `/keys` screen's list three rows for a wheel step while
    /// browsing; Capture and Clash swallow through the wrapper. The
    /// router swallows the wheel over the notice overlay's own slab
    /// before calling. Away from `bindings_press`.
    pub(crate) fn keys_screen_wheel(&mut self, up: bool) -> Effect {
        let height = self.keys_height();
        if let Some(screen) = self.keyboard.screen.as_mut() {
            let _ = screen.wheel(up, &self.keyboard.keys, height);
        }
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

    /// Runs an action: `new_session` leaves for home the way `/new`
    /// does, and `go_home` the way `/home` does, on the on-screen
    /// session's row. Both drop focus and any steering selection first,
    /// so the entry bar or the row takes focus. `session_only` chooses
    /// in the open model picker for this session only, or saves the open
    /// checklist as `scoped_models`. Anything else does nothing.
    pub(super) fn on_action(&mut self, id: &'static str) -> Effect {
        if id == "session_only" {
            if self.top_surface() != Surface::Picker {
                return Effect::None;
            }
            if self.credential_view_open() {
                return self.credential_session_only();
            }
            return self.model_picker_session_only_inner();
        }
        if !matches!(id, "new_session" | "go_home") {
            return Effect::None;
        }
        self.history.cancel();
        self.armed_at = None;
        self.copied = false;
        self.focus = None;
        self.steering.clear(&mut self.draft);
        let effect = if id == "go_home" {
            self.leave_on_row()
        } else {
            self.leave()
        };
        self.edited();
        effect
    }
}

#[cfg(test)]
#[path = "keyboard_screen_tests.rs"]
mod screen_tests;

#[cfg(test)]
#[path = "keyboard_tests.rs"]
mod tests;
