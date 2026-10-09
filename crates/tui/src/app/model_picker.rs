//! The model picker on the app: the catalogue it lists, the reads it
//! owes the loop, and the choices it sends (`docs/tui.md`, "Swapped
//! views"). Opening and keys route here; choosing sends one `model`
//! command, and attached choices write only when the session accepts it.

use super::App;
use crate::catalogue::{Catalogue, Refresh};
use crate::configure::Layer;
use crate::keys::{Edit, Key};
use crate::model_picker::{Choice, Mode, command_args, saves};
use crate::swapped::{Frame, Spot, about, rows_height};

/// The thinking levels `/thinking` takes, in order.
const LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

impl App {
    /// Folds a model-list read's answer: each catalogue notice shows once,
    /// and a read error shows once with the old catalogue kept.
    pub(crate) fn on_models(&mut self, result: Result<Catalogue, String>) {
        match &result {
            Ok(catalogue) => {
                for notice in &catalogue.notices {
                    self.push_notice(notice.clone());
                }
            }
            Err(error) => {
                self.push_notice(error.clone());
            }
        }
        self.model_picker.store(result);
    }

    /// The model-list read the loop owes, if one is owed.
    pub(crate) fn take_reads(&mut self) -> Option<Refresh> {
        self.model_picker.take_read()
    }

    /// Opens the model picker: each open starts fresh, on the on-screen
    /// model's row, else the first row. On home the pending session-only
    /// choice names it, else the chips; attached, the panel fold does.
    /// Each open asks `Stale`. One swapped view shows at a time, so
    /// another swapped view closes.
    pub(crate) fn open_model_picker(&mut self, mode: Mode) -> super::Effect {
        self.close_config_view();
        self.close_session_view();
        let on_screen = if self.on_home() {
            self.home_model()
        } else {
            self.panel_state.model().map(|model| {
                (
                    model.to_owned(),
                    self.panel_state.thinking().map(str::to_owned),
                )
            })
        };
        self.model_picker.open(
            mode,
            on_screen
                .as_ref()
                .map(|(model, level)| (model.as_str(), level.as_deref())),
        );
        super::Effect::None
    }

    /// A click on the open picker's `spot`: the ✕ closes it, the buttons
    /// act as their keys, a row selects, and a name or chip cell chooses.
    /// A click always saves: `s` is the only path to a session-only
    /// choice.
    pub(crate) fn model_picker_click(&mut self, spot: Spot) -> super::Effect {
        match spot {
            Spot::Close => {
                self.model_picker.close();
                super::Effect::None
            }
            Spot::Row(at) => {
                self.model_picker.select_frame_row(at);
                super::Effect::None
            }
            Spot::Cell(row, cell) => match self.model_picker.click_cell(row, cell) {
                Some(choice) => self.choose(choice),
                None => super::Effect::None,
            },
            // The picker draws no switches or rule rows.
            Spot::Switch { .. } | Spot::Revoke(_) => super::Effect::None,
        }
    }

    /// Sends `choice`: attached, one `model` command with its writes
    /// waiting on the session's answer; on home, the writes at once, or
    /// the next `start` carrying a session-only choice. The picker closes
    /// either way. With the hub not up, nothing is sent and it stays open.
    pub(crate) fn choose(&mut self, choice: Choice) -> super::Effect {
        let Some(session) = self.session().cloned() else {
            return self.choose_on_home(choice);
        };
        if self.link != super::Link::Up {
            self.push_notice("The hub is not connected; nothing changed.".to_owned());
            return super::Effect::None;
        }
        // A switch rebuilds the prompt cache: say its size before the
        // command goes out, only when the model or level changes and the
        // session on screen has a known last call.
        let changed = Some(choice.reference.as_str()) != self.panel_state.model()
            || choice.level.as_deref() != self.panel_state.thinking();
        if changed && let Some(tokens) = self.usage_on_screen() {
            self.push_notice(format!(
                "switching rebuilds the cache: about {} tokens",
                about(tokens)
            ));
        }
        let id = super::mint();
        let line =
            super::session_command(&id, "model", &session, Some(command_args(&choice))).to_string();
        self.pending.insert(
            id.clone(),
            (super::Kind::Command, crate::input::Draft::default()),
        );
        let writes = saves(&choice);
        if !writes.is_empty() {
            // With no seam nothing can ever be written: the switch still
            // goes out, for this session only.
            match self.configure_seam() {
                None => self.push_notice(
                    "Saving is not available; the switch is for this session only.".to_owned(),
                ),
                Some(_) => {
                    self.model_picker.awaiting.insert(id.clone(), writes);
                }
            }
        }
        self.model_picker.close();
        super::Effect::Send(vec![line])
    }

    /// Chooses on home, with no session to command: a saved choice
    /// writes at once and sets the home chips; a session-only choice
    /// rides the next `start` and saves nothing.
    fn choose_on_home(&mut self, choice: Choice) -> super::Effect {
        if choice.session_only {
            self.model_picker.start_model = Some(choice);
            self.model_picker.close();
            return super::Effect::None;
        }
        let workspace = self.workspace();
        match self.configure_seam() {
            None if !saves(&choice).is_empty() => self.push_notice(
                "Saving is not available; the switch is for this session only.".to_owned(),
            ),
            None => {}
            Some(seam) => {
                for (key, text) in saves(&choice) {
                    match seam.set(&workspace, Layer::Global, &key, &text) {
                        Ok(_) => {
                            // A saved model supersedes the session-only
                            // choice the next `start` would carry.
                            if key == "model" {
                                self.model_picker.start_model = None;
                            }
                            self.picker_saved(&key, &choice)
                        }
                        Err(error) => self.push_notice(format!("Saving {key} failed: {error}")),
                    }
                }
            }
        }
        self.model_picker.close();
        super::Effect::None
    }

    /// Folds a successful write into the picker's own copies: the
    /// catalogue's configured level and the home chips. Each changes
    /// only when its write succeeds.
    fn picker_saved(&mut self, key: &str, choice: &Choice) {
        if key == "model" {
            if let Some(home) = self.home.as_mut() {
                home.launch.model = Some(choice.reference.clone());
                // A model with no levels never shows one.
                if choice.level.is_none() {
                    home.launch.thinking = None;
                }
            }
            return;
        }
        let Some(reference) = key
            .strip_prefix("models.\"")
            .and_then(|key| key.strip_suffix("\".thinking"))
        else {
            return;
        };
        self.picker_thinking_saved(reference, choice.level.clone());
        if let Some(home) = self.home.as_mut() {
            home.launch.thinking = choice.level.clone();
        }
    }

    /// Writes the session-only choice on home into the `start` args: the
    /// model exactly, and the level as a per-run override, outranking
    /// every file.
    pub(in crate::app) fn with_start_model(&mut self, args: &mut serde_json::Value) {
        let Some(choice) = self.model_picker.start_model.as_ref() else {
            return;
        };
        let (model, override_text) = super::super::model_picker::start_args(choice);
        if let Some(object) = args.as_object_mut() {
            object.insert("model".to_owned(), serde_json::Value::String(model));
            if let Some(override_text) = override_text {
                object.insert(
                    "overrides".to_owned(),
                    serde_json::Value::Array(vec![serde_json::Value::String(override_text)]),
                );
            }
        }
    }

    /// The session accepted the `model` command `id`: its waiting writes
    /// go out, each in order. Any other id changes nothing.
    pub(in crate::app) fn model_picker_accepted(&mut self, id: &str) {
        let Some(writes) = self.model_picker.awaiting.remove(id) else {
            return;
        };
        let Some(seam) = self.configure_seam() else {
            return;
        };
        let workspace = self.workspace();
        for (key, text) in writes {
            match seam.set(&workspace, Layer::Global, &key, &text) {
                Ok(_) => {
                    // A saved model supersedes the session-only choice
                    // the next `start` would carry.
                    if key == "model" {
                        self.model_picker.start_model = None;
                    }
                    self.picker_accepted(&key, &text)
                }
                Err(error) => {
                    self.push_notice(format!("Saving {key} failed: {error}"));
                }
            }
        }
    }

    /// Folds a write the session's acceptance made into the picker's own
    /// copies: the catalogue's configured level. The global `model` needs
    /// no copy: the panel fold carries the session's own.
    fn picker_accepted(&mut self, key: &str, text: &str) {
        let Some(reference) = key
            .strip_prefix("models.\"")
            .and_then(|key| key.strip_suffix("\".thinking"))
        else {
            return;
        };
        self.picker_thinking_saved(reference, Some(text.to_owned()));
    }

    /// Folds a saved level into the catalogue and matching pending start.
    fn picker_thinking_saved(&mut self, reference: &str, level: Option<String>) {
        for entry in &mut self.model_picker.catalogue.models {
            if entry.reference == reference {
                entry.configured = level.clone();
            }
        }
        if let Some(choice) = self.model_picker.start_model.as_mut()
            && choice.reference == reference
        {
            choice.level = level;
        }
    }

    /// The session or the hub refused the `model` command `id`: its
    /// waiting writes are dropped, so a later acceptance for it writes
    /// nothing.
    pub(in crate::app) fn model_picker_rejected(&mut self, id: &str) {
        self.model_picker.awaiting.remove(id);
    }

    /// Runs `/thinking`: bare, the picker on the current model's chips;
    /// with a level, that level saved for the session's model, never the
    /// global default. The draft is consumed either way.
    pub(crate) fn thinking_command(&mut self, rest: &str) -> super::Effect {
        self.draft.clear();
        let mut words = rest.split_whitespace();
        let Some(level) = words.next() else {
            return self.open_thinking();
        };
        if !LEVELS.contains(&level) {
            return self.refuse_level(level);
        }
        if let Some(extra) = words.next() {
            return self.refuse_level(extra);
        }
        self.set_level(level)
    }

    /// Opens the picker on the current model's level chips, its row
    /// touched: Enter saves the level, `s` applies it to this session
    /// only. With no current model, or no catalogue entry for it, the
    /// picker opens as an ordinary choose.
    fn open_thinking(&mut self) -> super::Effect {
        let current = self.thinking_model();
        let listed = current.as_ref().is_some_and(|(model, _)| {
            self.model_picker
                .catalogue
                .models
                .iter()
                .any(|entry| &entry.reference == model)
        });
        if listed {
            self.open_model_picker(Mode::Thinking)
        } else {
            self.open_model_picker(Mode::Choose)
        }
    }

    /// Sets `level` for the session's model, saving only the level, as
    /// Enter on its chip does. Attached with no catalogue entry the
    /// command goes out and the session's rejection is the refusal; on
    /// home with no entry the read is asked for instead.
    fn set_level(&mut self, level: &str) -> super::Effect {
        let Some((model, _)) = self.thinking_model() else {
            self.push_notice("Choose a model first: Ctrl+L.".to_owned());
            return super::Effect::None;
        };
        let declared = self
            .model_picker
            .catalogue
            .models
            .iter()
            .find(|entry| entry.reference == model)
            .map(|entry| entry.levels.clone());
        match declared {
            Some(levels) if !levels.iter().any(|declared| declared == level) => {
                if levels.is_empty() {
                    self.push_notice(format!("{model} takes no thinking level."));
                } else {
                    self.push_notice(format!("{model} takes {}.", levels.join(", ")));
                }
                super::Effect::None
            }
            Some(_) => self.choose(Choice {
                reference: model,
                level: Some(level.to_owned()),
                level_chosen: true,
                save_model: false,
                session_only: false,
            }),
            None if self.session().is_some() => self.choose(Choice {
                reference: model,
                level: Some(level.to_owned()),
                level_chosen: true,
                save_model: false,
                session_only: false,
            }),
            None => {
                self.push_notice(
                    "The model list is not read yet; try again in a moment.".to_owned(),
                );
                let want = self.model_picker.want;
                self.model_picker.want =
                    Some(want.map_or(Refresh::Cached, |want| want.max(Refresh::Cached)));
                super::Effect::None
            }
        }
    }

    /// The model `/thinking` acts on: the session's, or the pending home
    /// choice's, else the home chips'.
    fn thinking_model(&self) -> Option<(String, Option<String>)> {
        if self.on_home() {
            self.home_model()
        } else {
            self.panel_state.model().map(|model| {
                (
                    model.to_owned(),
                    self.panel_state.thinking().map(str::to_owned),
                )
            })
        }
    }

    /// The model and level on home: the pending session-only choice the
    /// next `start` carries, else the home chips.
    fn home_model(&self) -> Option<(String, Option<String>)> {
        if let Some(choice) = self.model_picker.start_model.as_ref() {
            return Some((choice.reference.clone(), choice.level.clone()));
        }
        self.home.as_ref().and_then(|home| {
            home.launch
                .model
                .clone()
                .map(|model| (model, home.launch.thinking.clone()))
        })
    }

    /// Refuses an unknown thinking word with the seven levels.
    fn refuse_level(&mut self, word: &str) -> super::Effect {
        self.push_notice(format!(
            "Unknown thinking level \"{word}\"; the levels are {}.",
            LEVELS.join(", ")
        ));
        super::Effect::None
    }
    /// size of the session on screen, if its last call is known. `None`
    /// while the picker is closed.
    pub(crate) fn model_picker_frame(&self, height: usize) -> Option<Frame> {
        self.model_picker.frame(height, self.usage_on_screen())
    }
    /// Whether the model picker is open.
    pub(crate) fn model_picker_open(&self) -> bool {
        self.model_picker.is_open()
    }

    /// A key for the open picker; `None` while it is closed, for Ctrl+C,
    /// for the global actions' keys, and while the quit question is up,
    /// so quitting and every global action keep their keys. Enter chooses
    /// and saves, `s` chooses for this session only. Every other key but
    /// the picker's own does nothing, and no key cycles.
    pub(in crate::app) fn model_picker_key(&mut self, key: &Key) -> Option<super::Effect> {
        if !self.model_picker_open() || self.quit_open() {
            return None;
        }
        match key {
            Key::CtrlC => None,
            // A global action acts in every context, the picker's
            // included: its key passes through to its handler.
            Key::F1
            | Key::CtrlO
            | Key::End
            | Key::AltA
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_) => None,
            Key::Up => {
                self.model_picker.move_row(-1);
                Some(super::Effect::None)
            }
            Key::Down => {
                self.model_picker.move_row(1);
                Some(super::Effect::None)
            }
            Key::Tab => {
                self.model_picker.toggle_show_all();
                Some(super::Effect::None)
            }
            Key::CtrlR => {
                self.model_picker.refresh();
                Some(super::Effect::None)
            }
            Key::PageUp | Key::PageDown => {
                // A page is the rows the list shows: the view's height
                // less its header, status and footer.
                let height = usize::from(if self.on_home() {
                    self.screen.height()
                } else {
                    u16::try_from(self.conversation_height()).unwrap_or(u16::MAX)
                });
                let shown = self
                    .model_picker
                    .frame(height, self.usage_on_screen())
                    .map(|frame| rows_height(&frame, height))
                    .unwrap_or(height);
                self.model_picker.move_page(key, shown);
                Some(super::Effect::None)
            }
            Key::Esc => {
                self.model_picker.close();
                Some(super::Effect::None)
            }
            Key::Enter => {
                // With no scoped row to choose the picker stays open,
                // sending nothing; the key never reaches the draft.
                Some(match self.model_picker.choice(false) {
                    Some(choice) => self.choose(choice),
                    None => super::Effect::None,
                })
            }
            Key::Char('s') => {
                // `s` is the only path to a session-only choice: with no
                // scoped row it stays open, sending nothing.
                Some(match self.model_picker.choice(true) {
                    Some(choice) => self.choose(choice),
                    None => super::Effect::None,
                })
            }
            // Every other key does nothing, and no key cycles.
            Key::Char(_)
            | Key::Backspace
            | Key::BackTab
            | Key::CtrlG
            | Key::CtrlF
            | Key::CtrlV
            | Key::CtrlL
            | Key::AltUp
            | Key::AltDown
            | Key::AltX => Some(super::Effect::None),
        }
    }

    /// An edit for the open picker: the arrows move the selected row's
    /// chip, and every other edit does nothing. `None` while it is closed,
    /// while the quit question is up, and while the key map is open above
    /// it, so nothing reaches the picker underneath.
    pub(in crate::app) fn model_picker_edit(&mut self, edit: &Edit) -> Option<super::Effect> {
        if !self.model_picker_open() || self.quit_open() || self.keymap_top().is_some() {
            return None;
        }
        match edit {
            Edit::Left => self.model_picker.move_chip(-1),
            Edit::Right => self.model_picker.move_chip(1),
            Edit::ShiftEnter
            | Edit::CtrlJ
            | Edit::WordLeft
            | Edit::WordRight
            | Edit::DeleteWord
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Delete
            | Edit::Paste(_) => {}
        }
        Some(super::Effect::None)
    }
}

#[cfg(test)]
#[path = "model_picker_tests.rs"]
mod tests;
