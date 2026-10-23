//! The surfaces on top of the screen, in the one order that decides which
//! of them a key, Esc, the wheel, a click or the draw reaches first
//! (`docs/tui.md`, "Keys", "Contexts", "Overlays"). The order is derived
//! from the surfaces' own open state on every input; nothing keeps it.

use super::{App, Effect};
use crate::approvals::PanelKey;
use crate::keys::Key;
use crate::keyset::Context;
use ratatui::layout::Rect;
use std::cell::RefCell;

/// One surface that can hold the keyboard, topmost first: declaration
/// order is the precedence, so a surface compares less than every
/// surface below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Surface {
    /// The quit question, over every other surface.
    Quit,
    /// The accepted-import report.
    Report,
    /// The `/keys` screen.
    KeysScreen,
    /// The key map.
    Keymap,
    /// The model picker or the credential picker.
    Picker,
    /// A configuration view other than the credential picker.
    ConfigView,
    /// A session view.
    SessionView,
    /// Home's delete question or import offer.
    DeleteQuestion,
    /// Home's workspace picker.
    WorkspacePicker,
    /// The approval or question panel.
    Panel,
    /// The repository offer.
    Offer,
    /// The Ctrl+R history search panel.
    Search,
    /// The search results view.
    Results,
    /// The search bar.
    Find,
    /// Focus in the conversation.
    Focus,
    /// The `/` or `@` completion panel.
    Completion,
    /// A running `tty` job's view.
    Job,
    /// The notice overlay, a notice's whole text or the list.
    Notice,
    /// The input box, always last.
    Draft,
}

impl Surface {
    /// Whether a left press starting outside the surface's frame closes
    /// it as Esc does (`docs/tui.md`, "Bindings"). Every surface that
    /// draws a frame of its own over the screen is in the set; the
    /// panel, the offer and the other surfaces never take the click's
    /// place.
    // debt: explicit set stays; widen the matches! if the surveyor rules Panel/Offer in, never delete the gate while note calls decide coverage.
    pub(crate) fn closes_on_outside_click(self) -> bool {
        matches!(
            self,
            Surface::Quit
                | Surface::Report
                | Surface::Keymap
                | Surface::Picker
                | Surface::ConfigView
                | Surface::DeleteQuestion
                | Surface::WorkspacePicker
                | Surface::Search
                | Surface::Completion
                | Surface::Notice
        )
    }

    /// Whether Esc reaches the notice overlay before this surface does.
    /// The `/keys` screen yields the same way in `on_press`, and the
    /// conversation's focus inside its own arm, because it runs
    /// `cancel_pending_turn` after the overlay closes.
    pub(in crate::app) fn yields_esc_to_notice(self) -> bool {
        matches!(self, Surface::Offer | Surface::Results | Surface::Find)
    }
}

/// The slabs the last frame drew for its framed surfaces, keyed by
/// surface: what a click outside the overlay on top reads
/// (`docs/tui.md`, "Bindings"). The draw writes it and a later input
/// reads it, so it shares the interior-mutability shape of the other
/// frame-written state; `render` clears it before anything else, and a
/// surface is noted once per frame from the `Rect` its draw returned.
#[derive(Default)]
pub(crate) struct Frames(RefCell<Vec<(Surface, Rect)>>);

impl Frames {
    /// Drops the last frame's slabs, before the next frame notes its own.
    pub(crate) fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    /// Notes `surface`'s slab, replacing its earlier note; an empty slab
    /// notes nothing, so a frame squeezed to nothing takes the old path.
    // debt: replace branch stays as double-note insurance; drop it only if notes become provably once-per-frame by construction.
    pub(crate) fn note(&self, surface: Surface, slab: Rect) {
        if slab.is_empty() {
            return;
        }
        let mut frames = self.0.borrow_mut();
        if let Some(slot) = frames.iter_mut().find(|(open, _)| *open == surface) {
            slot.1 = slab;
        } else {
            frames.push((surface, slab));
        }
    }

    /// `surface`'s slab in the last frame, if it drew one.
    pub(crate) fn slab_of(&self, surface: Surface) -> Option<Rect> {
        self.0
            .borrow()
            .iter()
            .find(|(open, _)| *open == surface)
            .map(|(_, slab)| *slab)
    }
}

/// The open surfaces of `order` strictly below `above` and down to
/// `through`, both optional: `None` for `above` starts at the top, `None`
/// for `through` runs to the end.
pub(super) fn between(
    order: &[Surface],
    above: Option<Surface>,
    through: Option<Surface>,
) -> &[Surface] {
    let from = above.map_or(0, |at| order.partition_point(|open| *open <= at));
    let to = through.map_or(order.len(), |at| order.partition_point(|open| *open <= at));
    order.get(from..to).unwrap_or_default()
}

impl App {
    /// The slabs the last frame drew for its framed surfaces.
    pub(crate) fn frames(&self) -> &Frames {
        &self.frames
    }

    /// The surfaces open now, topmost first, ending with the input box.
    pub(crate) fn top_surfaces(&self) -> Vec<Surface> {
        let open = [
            (Surface::Quit, self.quit_open()),
            (Surface::Report, self.import_report_open()),
            (Surface::KeysScreen, self.keys_screen_open()),
            (Surface::Keymap, self.keymap_top().is_some()),
            (
                Surface::Picker,
                self.model_picker_open() || self.credential_view_open(),
            ),
            (
                Surface::ConfigView,
                self.config_view_open() && !self.credential_view_open(),
            ),
            (Surface::SessionView, self.session_view_open()),
            (Surface::DeleteQuestion, self.home_question_open()),
            (Surface::WorkspacePicker, self.home_picker_open()),
            (Surface::Panel, self.queue.open()),
            (Surface::Offer, self.offer_open()),
            (Surface::Search, self.search_panel().is_some()),
            (Surface::Results, self.results_open()),
            (Surface::Find, self.find_open()),
            (Surface::Focus, self.focus.is_some()),
            (Surface::Completion, self.completions().is_some()),
            (Surface::Job, self.item_job_target().is_some()),
            (Surface::Notice, self.notice_overlay().is_some()),
        ];
        open.into_iter()
            .filter_map(|(surface, open)| open.then_some(surface))
            .chain([Surface::Draft])
            .collect()
    }

    /// The surfaces that draw a frame of their own over every view, in
    /// the order they draw: [`Self::top_surfaces`] from the bottom up, so
    /// the one on top draws last, as far as they are open.
    pub(crate) fn framed_surfaces(&self) -> impl Iterator<Item = Surface> {
        self.top_surfaces()
            .into_iter()
            .rev()
            .filter(|surface| matches!(surface, Surface::Report | Surface::Quit))
    }

    /// The surface on top: the head of [`Self::top_surfaces`].
    pub(crate) fn top_surface(&self) -> Surface {
        self.top_surfaces()
            .first()
            .copied()
            .unwrap_or(Surface::Draft)
    }

    /// The context a stroke arrives in: the one the surface on top gives.
    /// The `/keys` screen takes its strokes before the bindings resolve,
    /// so only Ctrl+C reaches them, in the context of what lies below it.
    pub(super) fn key_context(&self) -> Context {
        self.top_surfaces()
            .into_iter()
            .find(|surface| *surface != Surface::KeysScreen)
            .map_or(Context::Input, |surface| self.context_of(surface))
    }

    /// The context `surface` gives while it is on top.
    fn context_of(&self, surface: Surface) -> Context {
        match surface {
            Surface::Quit
            | Surface::Report
            | Surface::Keymap
            | Surface::ConfigView
            | Surface::SessionView
            | Surface::DeleteQuestion
            | Surface::WorkspacePicker
            | Surface::Panel
            | Surface::Offer
            | Surface::Search
            | Surface::Completion
            | Surface::Notice => Context::Overlay,
            Surface::Picker => Context::Picker,
            Surface::Results | Surface::Find => Context::Search,
            Surface::Focus if self.focused_chip().is_some() => Context::Input,
            Surface::Focus => Context::Conversation,
            Surface::KeysScreen | Surface::Job | Surface::Draft => {
                if self.steering.is_selected() {
                    Context::Steering
                } else {
                    Context::Input
                }
            }
        }
    }

    /// Offers `key` to the open `surfaces` in order; the first that
    /// handles it wins. Esc reaches the notice overlay first at the
    /// surfaces that yield it.
    pub(super) fn route_to(&mut self, surfaces: &[Surface], key: &Key) -> Option<Effect> {
        surfaces
            .iter()
            .find_map(|surface| self.surface_key(*surface, key))
    }

    /// `key` at `surface`: its arm, or the notice overlay closing first
    /// for an Esc the surface yields. `None` when the surface leaves the
    /// key to the ones below.
    fn surface_key(&mut self, surface: Surface, key: &Key) -> Option<Effect> {
        if *key == Key::Esc && surface.yields_esc_to_notice() && self.notices.close() {
            return Some(Effect::None);
        }
        match surface {
            Surface::Quit => self.quit_key(key),
            Surface::Report => self.report_key(key),
            // The screen takes its strokes in `on_press`, and the
            // completion panels in `route_key`, between two slices.
            Surface::KeysScreen | Surface::Completion => None,
            Surface::Keymap => self.keymap_key(key),
            Surface::Picker => self
                .model_picker_key(key)
                .or_else(|| self.config_view_key(key)),
            Surface::ConfigView => self.config_view_key(key),
            Surface::SessionView => self.session_view_key(key),
            Surface::DeleteQuestion => self.question_key(key),
            Surface::WorkspacePicker => self.picker_key(key),
            Surface::Panel => match self.queue.on_key(key) {
                Some(PanelKey::Handled) => Some(Effect::None),
                Some(PanelKey::Answer) => Some(self.answer()),
                Some(PanelKey::Decline) => Some(self.decline()),
                None => None,
            },
            Surface::Offer => self.offer_key(key),
            Surface::Search => self.history_key(key),
            Surface::Results => self.results_key(key),
            Surface::Find => self.find_key(key),
            Surface::Focus => self.focus_key(key),
            Surface::Job => self.item_job_key(key),
            Surface::Notice => (*key == Key::Esc && self.notices.close()).then_some(Effect::None),
            Surface::Draft => Some(self.input_key(key)),
        }
    }

    /// Esc clears a selection shown, after the notice overlay closes
    /// first, and before anything it would interrupt.
    pub(super) fn selection_esc(&mut self, key: &Key) -> Option<Effect> {
        if *key == Key::Esc && self.select.span().is_some() && self.notices.close() {
            return Some(Effect::None);
        }
        self.select_key(key)
    }
}

#[cfg(test)]
#[path = "top_tests.rs"]
pub(in crate::app) mod tests;
