//! The model picker on the app: the catalogue it lists, the reads it
//! owes the loop, and the choices it sends (`docs/tui.md`, "Swapped
//! views"). Opening and keys route here; choosing sends one `model`
//! command, and attached choices write only when the session accepts it.

use super::App;
use crate::catalogue::{Catalogue, Refresh};
use crate::configure::{ConfigureError, ProviderInstall};
use crate::keys::{Edit, Key};
use crate::model_picker::{
    Choice, InstallStart, Landed, Mode, Progress, command_args, saves, scoped_save,
};
use crate::swapped::{Spot, about};
use contract::ThinkingLevel;

impl App {
    /// Folds a model-list read's answer: the catalogue's notices show
    /// grouped by code, and a read error shows once with the old
    /// catalogue kept.
    pub(crate) fn on_models(&mut self, result: Result<Catalogue, String>) {
        // The load landed, answered or not: its wait is over.
        self.notices
            .clear_code("models_loading", Self::MODELS_LOADING_TEXT);
        match &result {
            Ok(catalogue) => {
                for notice in contract::events::group_notices(
                    catalogue.notices.clone(),
                    contract::events::grouped_lead,
                ) {
                    self.push_notice(&crate::format::code(&notice.code), notice.message.clone());
                }
            }
            Err(error) => {
                self.push_notice("io_failed", error.clone());
            }
        }
        self.model_picker.store(result);
        self.keep_model_picker_top(false);
    }

    /// Counts the startup catalogue read as owed, so its answer never
    /// drops a meanwhile opened picker's mark.
    pub(crate) fn note_catalogue_read(&mut self) {
        self.model_picker.note_asked();
    }

    /// The model-list read the loop owes, if one is owed. On a remote
    /// terminal the hub's catalogue reads use the workspace in use, stored
    /// just before the read is returned; with no read owed nothing is stored.
    pub(crate) fn take_reads(&mut self) -> Option<Refresh> {
        let read = self.model_picker.take_read();
        if read.is_some()
            && let Some(remote) = self.remote()
        {
            // None stores none: the next `models()` reads at the hub's
            // global file, as a cold read does.
            remote.set_workspace(self.workspace());
        }
        read
    }

    /// Runs the owed start pick: opening Choose, asking the open's
    /// read now (`Stale`, or the read the open owes), and installing
    /// the answer. The pick waits for the chips: it opens only when
    /// the hub named no model, and a named one drops it. A remote
    /// terminal with no hub yet keeps the pick owed: the sync read
    /// would park on the link the loop itself brings up. A local home
    /// runs at once, as before. With no reader the open still shows;
    /// a models error is a notice as today, opening nothing.
    pub(crate) fn start_model_pick(&mut self) -> Vec<String> {
        let remote = self.remote().is_some();
        if remote && !self.outbox.is_up() {
            self.owe_start_pick();
            return Vec::new();
        }
        if !remote && self.outbox.is_up() {
            // A local home picks once at startup, before the link
            // exists: later hellos must not reopen it.
            return Vec::new();
        }
        if remote && self.chip_read_failed() {
            // The chips failed: the pick stays owed until a new
            // workspace or link reads them.
            return Vec::new();
        }
        if remote && !self.take_start_pick() {
            return Vec::new();
        }
        if remote
            && self
                .home
                .as_ref()
                .and_then(|home| home.launch.model.as_deref())
                .is_some()
        {
            // The hub named the chip's model: the pick is taken and
            // dropped, never opened.
            return Vec::new();
        }
        // The open reads for itself: on the loop thread take its read
        // and ask it now, so the loop owes the thread nothing more.
        // At startup the loop does not exist yet, so a local open only
        // opens: the cached read the loop asks after the first frame
        // fills it, and the first frame draws at once. With no reader
        // the open still shows.
        self.open_model_picker(Mode::Choose);
        if !remote {
            return Vec::new();
        }
        let Some(read) = self.models_reader() else {
            return Vec::new();
        };
        // The open read for itself: take its read through the loop's
        // operation, recording the workspace the hub's catalogue read
        // uses, as `take_reads` does.
        let refresh = self.take_reads().unwrap_or(Refresh::Stale);
        match read(refresh) {
            Ok(catalogue) => self.on_models(Ok(catalogue)),
            Err(error) => {
                self.on_models(Err(error));
                self.model_picker.close();
            }
        }
        Vec::new()
    }

    /// Opens the model picker: each open starts fresh, on the on-screen
    /// model's row, else the first row. On home the pending session-only
    /// choice names it, else the chips; attached, the panel fold does.
    /// Each open asks `Stale`. One swapped view shows at a time, so
    /// another swapped view closes. A fresh open reads for itself, so
    /// an owed start pick drops: the open's own read supersedes it.
    pub(crate) fn open_model_picker(&mut self, mode: Mode) -> super::Effect {
        self.close_config_view();
        self.close_session_view();
        let _ = self.take_start_pick();
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
        self.keep_model_picker_top(true);
        super::Effect::None
    }

    /// A click on the open picker's `spot`: the ✕ closes it, the buttons
    /// act as their keys, a row selects, and a name or chip cell chooses.
    /// A click always saves: Ctrl+S is the only path to a session-only
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
            Spot::Cell(row, cell) => {
                if let Some(provider) = self.model_picker.install_click(row, cell) {
                    return self.start_install(provider);
                }
                match self.model_picker.click_cell(row, cell) {
                    Some(choice) => self.choose(choice),
                    None => super::Effect::None,
                }
            }
            // The picker draws no switches, rule rows or item rows.
            Spot::Switch { .. } | Spot::Revoke(_) | Spot::Item(_) => super::Effect::None,
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
        if !self.outbox.is_up() {
            self.push_notice(
                "connection_failed",
                "The hub is not connected; nothing changed.".to_owned(),
            );
            return super::Effect::None;
        }
        // A switch rebuilds the prompt cache: say its size before the
        // command goes out, only when the model or level changes and the
        // session on screen has a known last call.
        let changed = Some(choice.reference.as_str()) != self.panel_state.model()
            || choice.level.as_deref() != self.panel_state.thinking();
        if changed && let Some(tokens) = self.usage_on_screen() {
            self.push_notice(
                "cache_rebuild",
                format!(
                    "switching rebuilds the cache: about {} tokens",
                    about(tokens)
                ),
            );
        }
        let id = super::mint();
        let line =
            super::session_command(&id, "model", &session, Some(command_args(&choice))).to_string();
        self.outbox.pending.insert(
            id.clone(),
            (super::Kind::Command, crate::input::Draft::default()),
        );
        // The send stashes its choice for the answer. A send while a
        // turn runs also says so up front: the chips hold the old model
        // and level until the session accepts it, so the notice says the
        // choice applies when the turn ends.
        self.model_picker
            .sent
            .insert(id.clone(), (choice.reference.clone(), choice.level.clone()));
        if self.turn_running() {
            self.push_pending_notice(&choice);
        }
        let writes = saves(&choice);
        if choice.session_only {
            // A session-only choice saves nothing: its session's accept
            // names the row instead.
            self.model_picker
                .only_pending
                .insert(id.clone(), (session.clone(), choice.reference.clone()));
        }
        if !writes.is_empty() {
            // With no seam nothing can ever be written: the switch still
            // goes out, for this session only.
            match self.configure_seam() {
                None => self.push_notice(
                    "io_failed",
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
        match self.configure_seam() {
            None if !saves(&choice).is_empty() => self.push_notice(
                "io_failed",
                "Saving is not available; the switch is for this session only.".to_owned(),
            ),
            None => {}
            Some(_) => {
                for (key, text) in saves(&choice) {
                    match self.write_global(&key, &text) {
                        Some(Ok(_)) => {
                            // A saved model supersedes the session-only
                            // choice the next `start` would carry.
                            if key == "model" {
                                self.model_picker.start_model = None;
                            }
                            self.picker_saved(&key, &choice)
                        }
                        Some(Err(error)) => {
                            self.push_notice("io_failed", format!("Saving {key} failed: {error}"))
                        }
                        // No workspace to address on a local terminal:
                        // the choice rides the next `start` unsaved.
                        None => {}
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
    /// every file. Bare `fiber`'s `--model` and `-c` stay: the choice
    /// supersedes the layer's `model=` and its own thinking key, while
    /// every unrelated key rides on (`docs/configuration.md`, "Layers":
    /// a later `-c` wins, and every `-c` wins over `--model`). The layer
    /// holds canonical keys, as `main` hands them, so each comparison is
    /// one plain string: `model`, and the choice's canonical thinking
    /// key.
    pub(in crate::app) fn with_start_model(&mut self, args: &mut serde_json::Value) {
        let Some(choice) = self.model_picker.start_model.as_ref() else {
            return;
        };
        let (model, override_text) = super::super::model_picker::start_args(choice);
        let thinking_key = canonical_thinking_key(&choice.reference);
        let merged: Vec<serde_json::Value> = args
            .get("overrides")
            .and_then(serde_json::Value::as_array)
            .map(|overrides| {
                overrides
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter(|override_text| {
                        // The choice names the model, so the layer's
                        // `model=` never stands beside it; its
                        // thinking key stands only without a chosen
                        // level.
                        let key = override_text
                            .split_once('=')
                            .map_or(*override_text, |(key, _)| key);
                        key != "model" && (choice.level.is_none() || key != thinking_key)
                    })
                    .map(|override_text| serde_json::Value::String(override_text.to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let mut merged = merged;
        if let Some(override_text) = override_text {
            merged.push(serde_json::Value::String(override_text));
        }
        if let Some(object) = args.as_object_mut() {
            object.insert("model".to_owned(), serde_json::Value::String(model));
            if merged.is_empty() {
                object.remove("overrides");
            } else {
                object.insert("overrides".to_owned(), serde_json::Value::Array(merged));
            }
        }
    }

    /// Whether a turn runs on screen: attached and busy, or a folded
    /// pass holding a busy turn. Only then does a sent choice wait for
    /// the turn's end, and only then does the send push its notice.
    fn turn_running(&self) -> bool {
        if let super::Phase::Attached { busy, .. } = &self.phase
            && *busy
        {
            return true;
        }
        matches!(
            &self.overlay,
            super::Overlay::Folding {
                busy: Some(true),
                ..
            }
        )
    }

    /// The pending notice's text for a choice: the level alone when only
    /// a level rides along for the model on screen, else the model with
    /// its level, else the model. Each says it applies when the turn
    /// ends, since the chips hold the old model and level until the
    /// session accepts it.
    fn pending_text(&self, reference: &str, level: Option<&str>) -> String {
        const END: &str = "applies when this turn ends";
        if let Some(level) = level
            && self.panel_state.model() == Some(reference)
        {
            return format!("thinking {level} {END}");
        }
        if let Some(level) = level {
            return format!("{reference} · {level} {END}");
        }
        format!("{reference} {END}")
    }

    /// Pushes the pending notice for a choice sent while a turn runs.
    fn push_pending_notice(&mut self, choice: &Choice) {
        let text = self.pending_text(&choice.reference, choice.level.as_deref());
        self.push_notice("switch_pending", text);
    }

    /// Dismisses the newest `switch_pending` notice when its text is
    /// exactly the stashed choice's wording: the rejection voids that
    /// claim, but another choice's notice stands.
    fn dismiss_pending_notice(&mut self, reference: &str, level: Option<&str>) {
        let text = self.pending_text(reference, level);
        let newest = self
            .notices
            .lines()
            .into_iter()
            .rev()
            .find_map(|line| match line {
                super::NoticeLine::Notice {
                    id,
                    code,
                    text: shown,
                } if code == "switch_pending" && shown == text => Some(id),
                super::NoticeLine::Notice { .. } | super::NoticeLine::More(_) => None,
            });
        if let Some(id) = newest {
            self.notices.dismiss(id);
        }
    }

    /// The session accepted the `model` command `id` from `session`: a
    /// session-only wait names its row, but only from its own session:
    /// a late accept after a switch, or another session's, is ignored.
    /// A saved choice's writes go out, each in order, and clear the
    /// session's session-only mark. Any other id changes nothing.
    pub(in crate::app) fn model_picker_accepted(
        &mut self,
        id: &str,
        session: &contract::SessionId,
    ) {
        // The send's stash drains: the choice applied, so no later
        // answer replaces anything for it.
        self.model_picker.sent.remove(id);
        if let Some((waiting, reference)) = self.model_picker.only_pending.remove(id) {
            if waiting == *session {
                self.model_picker.session_only = Some((waiting, reference));
            } else {
                self.model_picker
                    .only_pending
                    .insert(id.to_owned(), (waiting, reference));
            }
            return;
        }
        let Some(writes) = self.model_picker.awaiting.remove(id) else {
            return;
        };
        if self
            .model_picker
            .session_only
            .as_ref()
            .is_some_and(|(on, _)| on == session)
        {
            self.model_picker.session_only = None;
        }
        for (key, text) in writes {
            match self.write_global(&key, &text) {
                Some(Ok(_)) => {
                    // A saved model supersedes the session-only choice
                    // the next `start` would carry.
                    if key == "model" {
                        self.model_picker.start_model = None;
                    }
                    self.picker_accepted(&key, &text)
                }
                Some(Err(error)) => {
                    self.push_notice("io_failed", format!("Saving {key} failed: {error}"));
                }
                // No workspace to address on a local terminal: the write
                // is dropped, as a missing seam drops it.
                None => {}
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
    /// nothing, and its session-only wait is dropped with them. A
    /// stashed choice's pending notice goes with it: a rejection
    /// replaces it, and a choice from a dropped session takes its
    /// already dead claim with it. Returns whether the refusal still
    /// reports: a stashed choice with no waiting writes belonged to a
    /// dropped session, so it stays silent.
    pub(in crate::app) fn model_picker_rejected(&mut self, id: &str) -> bool {
        let writes = self.model_picker.awaiting.remove(id).is_some()
            || self.model_picker.only_pending.remove(id).is_some();
        let Some((reference, level)) = self.model_picker.sent.remove(id) else {
            // Never a `model` send: the generic path reports.
            return true;
        };
        self.dismiss_pending_notice(&reference, level.as_deref());
        writes
    }

    /// Drops a left session's session-only wait and mark, so a late
    /// accept after leaving sets nothing. Nothing while staying home.
    pub(crate) fn leave_session(&mut self, session: Option<contract::SessionId>) {
        if let Some(leaving) = session {
            self.model_picker
                .only_pending
                .retain(|_, (waiting, _)| *waiting != leaving);
            if self
                .model_picker
                .session_only
                .as_ref()
                .is_some_and(|(on, _)| *on == leaving)
            {
                self.model_picker.session_only = None;
            }
        }
    }

    /// Keeps an attached session's session-only wait and mark, dropping
    /// any other session's: attaching to another session forgets the
    /// last.
    pub(crate) fn switch_session(&mut self, session: &contract::SessionId) {
        self.model_picker
            .only_pending
            .retain(|_, (waiting, _)| waiting == session);
        if self
            .model_picker
            .session_only
            .as_ref()
            .is_some_and(|(on, _)| on != session)
        {
            self.model_picker.session_only = None;
        }
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
        if level.parse::<ThinkingLevel>().is_err() {
            return self.refuse_level(level);
        }
        if let Some(extra) = words.next() {
            return self.refuse_level(extra);
        }
        self.set_level(level)
    }

    /// Opens the picker on the current model's level chips, its row
    /// touched: Enter saves the level, Ctrl+S applies it to this session
    /// only. With no current model, or no catalogue entry for it, the
    /// picker opens as an ordinary choose.
    pub(in crate::app) fn open_thinking(&mut self) -> super::Effect {
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
            self.push_notice("no_model", "Choose a model first: Ctrl+L.".to_owned());
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
                    self.push_notice(
                        "invalid_arguments",
                        format!("{model} takes no thinking level."),
                    );
                } else {
                    self.push_notice(
                        "invalid_arguments",
                        format!("{model} takes {}.", levels.join(", ")),
                    );
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
                self.push_notice("models_loading", Self::MODELS_LOADING_TEXT.to_owned());
                let want = self.model_picker.want;
                self.model_picker.want =
                    Some(want.map_or(Refresh::Cached, |want| want.max(Refresh::Cached)));
                super::Effect::None
            }
        }
    }

    /// The models-wait notice's identity: the text the empty-list path pushes.
    /// The load landing ends the wait, answered or not.
    const MODELS_LOADING_TEXT: &str = "The model list is not read yet; try again in a moment.";

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

    /// Runs `/scoped-models`: the picker as a checklist over every
    /// installed model, each row marked from the saved list. Text after
    /// the command is ignored: the draft is cleared unread.
    pub(crate) fn scoped_models_command(&mut self) -> super::Effect {
        self.draft.clear();
        self.open_model_picker(Mode::Scope)
    }

    /// Saves the checklist: the marked references in catalogue order,
    /// then the old list's entries that are not installed, or `[]` when
    /// none is marked. The write goes through the configuration seam to
    /// the global file at once, on home and attached alike: no command
    /// is sent. Ctrl+S, the `session_only` action's key, runs it, so a
    /// rebind follows. The picker's list follows a successful write, scoping
    /// the next open. The picker closes either way.
    fn save_scoped(&mut self) -> super::Effect {
        let list = {
            let picker = &self.model_picker;
            let marks = picker
                .open
                .as_ref()
                .map(|open| open.marks.as_slice())
                .unwrap_or(&[]);
            scoped_save(&picker.catalogue.models, marks, &picker.scoped)
        };
        // A string list always serialises; the fallback keeps the save
        // total.
        let text = serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_owned());
        // `write_global` is `None` with no seam, so one match covers
        // both: with no seam, or no workspace to address on a local
        // terminal, nothing changed.
        match self.write_global("scoped_models", &text) {
            Some(Ok(_)) => {
                self.model_picker.scoped = list;
                if let Some(home) = self.home.as_mut() {
                    home.launch.scoped_models = self.model_picker.scoped.clone();
                }
            }
            Some(Err(error)) => {
                self.push_notice("io_failed", format!("Saving scoped_models failed: {error}"));
            }
            None => self.push_notice(
                "io_failed",
                "Saving is not available; nothing changed.".to_owned(),
            ),
        }
        self.model_picker.close();
        super::Effect::None
    }

    /// Refuses an unknown thinking word with the seven levels.
    fn refuse_level(&mut self, word: &str) -> super::Effect {
        self.push_notice(
            "invalid_arguments",
            format!(
                "Unknown thinking level \"{word}\"; the levels are {}.",
                ThinkingLevel::ALL.map(ThinkingLevel::as_str).join(", ")
            ),
        );
        super::Effect::None
    }
    /// The open picker's overlay view: the filter, the buttons, one
    /// section per shown provider, and the keys. `None` while closed.
    pub(crate) fn model_picker_view(&self) -> Option<crate::model_picker::PickerView> {
        let current;
        let on_screen = if self.on_home() {
            current = self.home_model().map(|(model, _)| model);
            current.as_deref()
        } else {
            self.panel_state.model()
        };
        let home_only = if self.on_home() {
            self.model_picker
                .start_model
                .as_ref()
                .map(|choice| choice.reference.as_str())
        } else {
            None
        };
        let view = self.model_picker.view(&crate::model_picker::PickerCtx {
            usage: self.usage_on_screen(),
            wall_ms: self.rail_state().wall(),
            spinner: self.motion().spinner(),
            current: on_screen,
            home_only,
        })?;
        // The spinner of a running install moves between inputs.
        if view
            .installs
            .iter()
            .any(|row| row.progress == Some(Progress::Installing))
        {
            self.motion().ask_frame();
        }
        Some(view)
    }
    /// The install the loop has not started yet, once.
    pub(crate) fn take_install_start(&mut self) -> Option<InstallStart> {
        self.model_picker.installs.take_start()
    }

    /// Starts installing `provider`: choosing its row is the approval, so
    /// nothing asks. One install runs at a time, so a choice while one
    /// runs does nothing. The row fails at once, with no job, when there
    /// is no seam, no hub link on a remote terminal (a dispatched call
    /// would wait for the link with no end), or no workspace to install
    /// into.
    fn start_install(&mut self, provider: String) -> super::Effect {
        if self.model_picker.installs.busy() {
            return super::Effect::None;
        }
        let Some(seam) = self.configure_seam() else {
            self.model_picker.installs.fail(
                &provider,
                "Installing is not available in this terminal.".to_owned(),
            );
            return super::Effect::None;
        };
        let remote = self.remote();
        if remote.is_some() && !self.outbox.is_up() {
            self.model_picker
                .installs
                .fail(&provider, "The hub is not connected.".to_owned());
            return super::Effect::None;
        }
        let workspace = match (self.workspace(), remote) {
            (Some(workspace), _) => workspace,
            // With no local workspace the hub-global parent stands in for
            // it, resolved here on the loop thread as `write_global` does.
            (None, Some(remote)) => match remote.global_parent() {
                Ok(parent) => parent,
                Err(error) => {
                    self.model_picker.installs.fail(&provider, error.message);
                    return super::Effect::None;
                }
            },
            (None, None) => {
                self.model_picker
                    .installs
                    .fail(&provider, "No workspace to install into.".to_owned());
                return super::Effect::None;
            }
        };
        self.model_picker.installs.start(InstallStart {
            provider,
            workspace,
            seam,
        });
        super::Effect::None
    }

    /// Folds an install's result. With the picker not showing its rows a
    /// failure is a notice under the error's own code, and a success
    /// pushes its credential lines as notices.
    pub(crate) fn on_install(
        &mut self,
        provider: String,
        result: Result<ProviderInstall, ConfigureError>,
    ) {
        let code = result
            .as_ref()
            .err()
            .map(|error| crate::format::code(&error.code))
            .filter(|code| !code.is_empty())
            .unwrap_or_else(|| "io_failed".to_owned());
        match self.model_picker.land_install(provider, result) {
            Landed::Installed { notices } => {
                for (code, text) in notices {
                    self.push_notice(code, text);
                }
            }
            Landed::Notice(text) => self.push_notice(&code, text),
            Landed::Nothing => {}
        }
    }

    /// Whether a provider installs on the hub now. The install holds the
    /// terminal's one hub connection, so the config views' asks wait
    /// behind it: they are refused instead.
    pub(in crate::app) fn installing(&self) -> bool {
        self.model_picker.installs.busy() && self.remote().is_some()
    }

    /// The refusal the config views give while a provider installs.
    pub(in crate::app) const INSTALLING_TEXT: &str =
        "a provider is installing; try again when it finishes";

    /// Whether the model picker is open.
    pub(crate) fn model_picker_open(&self) -> bool {
        self.model_picker.is_open()
    }

    /// The model picker's draw height below its one-row swapped-view
    /// header, matching the `area.height - 1` passed by `swapped::draw`.
    fn model_picker_area_height(&self) -> u16 {
        let area = if self.home.is_some() && self.session().is_none() {
            usize::from(self.screen.height())
        } else {
            self.conversation_area_height()
        };
        u16::try_from(area).unwrap_or(u16::MAX).saturating_sub(1)
    }

    /// Keeps the model-picker top within the current rendered body and
    /// keeps focus visible. A row-set replacement uses the focus-derived
    /// start; navigation and refresh retain the current top when possible.
    fn keep_model_picker_top(&mut self, reset: bool) {
        let Some(view) = self.model_picker_view() else {
            return;
        };
        let height = self.model_picker_area_height();
        let top = if reset {
            crate::view::model_picker::focus_window_top(&view, height)
        } else {
            crate::view::model_picker::keep_focus_visible_top(&view, height)
        };
        if let Some(open) = self.model_picker.open.as_mut() {
            open.top = top;
        }
    }

    /// Moves the model picker's body viewport by three rendered rows.
    /// The match selection stays separate and unchanged while find is open.
    pub(crate) fn model_picker_wheel(&mut self, up: bool) -> super::Effect {
        let Some(view) = self.model_picker_view() else {
            return super::Effect::None;
        };
        let height = self.model_picker_area_height();
        let (top, focus) = crate::view::model_picker::picker_wheel_window(&view, up, height);
        if let Some(open) = self.model_picker.open.as_mut() {
            open.top = top;
        }
        if let Some(focus) = focus {
            self.model_picker.select_frame_row(focus);
        }
        super::Effect::None
    }

    /// A key for the open picker; `None` while it is closed, for Ctrl+C
    /// and for the global actions' keys, so quitting and every global
    /// action keep their keys. Enter toggles
    /// the checklist's focused mark and types nothing; on a choosing
    /// picker Enter chooses and saves, Ctrl+S chooses for this session
    /// only. Every other key but the picker's own does nothing, and no
    /// key cycles.
    pub(in crate::app) fn model_picker_key(&mut self, key: &Key) -> Option<super::Effect> {
        if !self.model_picker_open() {
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
                self.keep_model_picker_top(false);
                Some(super::Effect::None)
            }
            Key::Down => {
                self.model_picker.move_row(1);
                self.keep_model_picker_top(false);
                Some(super::Effect::None)
            }
            Key::Tab => {
                let before = self.model_picker.open.as_ref().map(|open| open.show_all);
                self.model_picker.toggle_show_all();
                if self.model_picker.open.as_ref().map(|open| open.show_all) != before {
                    self.keep_model_picker_top(true);
                }
                Some(super::Effect::None)
            }
            Key::CtrlR => {
                self.model_picker.refresh();
                Some(super::Effect::None)
            }
            Key::PageUp | Key::PageDown => {
                // A page is the windowed models less one: the overlay's
                // body height in models.
                let height = self.model_picker_area_height();
                let step = self
                    .model_picker_view()
                    .map(|view| crate::view::model_picker::page_step(&view, height))
                    .unwrap_or(usize::from(height));
                self.model_picker.move_page(key, step);
                self.keep_model_picker_top(false);
                Some(super::Effect::None)
            }
            Key::Esc => {
                // The first Esc while a query is typed clears it, keeping
                // the picker open; the next closes it.
                let typed = !self.model_picker.query().is_empty();
                if typed {
                    self.model_picker.clear_query();
                    self.keep_model_picker_top(true);
                    Some(super::Effect::None)
                } else {
                    self.model_picker.close();
                    Some(super::Effect::None)
                }
            }
            Key::Enter => {
                // The checklist toggles its focused row's mark, keeping
                // the picker open; choosing sends one command. With no
                // scoped row to choose the picker stays open, sending
                // nothing; the key never reaches the draft.
                if self.model_picker.is_scope() {
                    self.model_picker.toggle_mark();
                    return Some(super::Effect::None);
                }
                // A focused not-installed row installs: choosing it is the
                // approval.
                if let Some(provider) = self.model_picker.install_target() {
                    return Some(self.start_install(provider));
                }
                Some(match self.model_picker.choice(false) {
                    Some(choice) => self.choose(choice),
                    None => super::Effect::None,
                })
            }
            Key::Char(c) => {
                // Typing filters the list on every letter, Space
                // included, in every mode.
                let before = self.model_picker.query().to_owned();
                self.model_picker.push_query(*c);
                if self.model_picker.query() != before {
                    self.keep_model_picker_top(true);
                }
                Some(super::Effect::None)
            }
            Key::Backspace => {
                // Backspace shortens the filter query in every mode.
                let before = self.model_picker.query().to_owned();
                self.model_picker.pop_query();
                if self.model_picker.query() != before {
                    self.keep_model_picker_top(true);
                }
                Some(super::Effect::None)
            }
            // Every other key does nothing, and no key cycles.
            Key::BackTab
            | Key::CtrlG
            | Key::CtrlF
            | Key::CtrlV
            | Key::CtrlL
            | Key::AltUp
            | Key::AltDown
            | Key::AltX => Some(super::Effect::None),
        }
    }

    /// Chooses in the open picker for this session only, through the
    /// `session_only` action: Ctrl+S chooses at once, sending one `model`
    /// command and saving nothing, or saves the open checklist as
    /// `scoped_models`. Nothing while the picker is closed. With no row
    /// to choose the picker stays open, sending nothing.
    pub(in crate::app) fn model_picker_session_only(&mut self) -> super::Effect {
        if !self.model_picker_open() {
            return super::Effect::None;
        }
        // A rebound `session_only` key saves the checklist too.
        let effect = if self.model_picker.is_scope() {
            self.save_scoped()
        } else {
            match self.model_picker.choice(true) {
                Some(choice) => self.choose(choice),
                None => super::Effect::None,
            }
        };
        self.edited();
        self.settle();
        effect
    }

    /// An edit for the open picker: the arrows move the selected row's
    /// chip, and every other edit does nothing. `None` while it is closed.
    pub(in crate::app) fn model_picker_edit(&mut self, edit: &Edit) -> Option<super::Effect> {
        if !self.model_picker_open() {
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
            | Edit::DeleteToLineStart
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Delete
            | Edit::Paste(_) => {}
        }
        Some(super::Effect::None)
    }
}

/// The choice's thinking key in canonical spelling: `models` holding the
/// reference holding `thinking`, quoting only a segment that holds a dot,
/// as `config`'s dotted-path display spells it. `main` hands the terminal
/// canonical keys, so this plain string supersedes the choice's own key
/// with no parsing here.
pub(crate) fn canonical_thinking_key(reference: &str) -> String {
    if reference.contains('.') {
        format!("models.\"{reference}\".thinking")
    } else {
        format!("models.{reference}.thinking")
    }
}

#[cfg(test)]
#[path = "model_picker_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "model_picker_install_tests.rs"]
mod install_tests;
