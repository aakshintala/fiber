//! Home's workspace chooser: the workspace picked for the next `start`,
//! whether it is in git, the new worktree switch and the workspace
//! picker above the box.

use std::path::PathBuf;

use serde_json::{Map, Value};

use super::super::{App, Effect, Sent, Waiting, hub_line};
use super::typed::{Resolved, Typed};
use super::{Home, typed};
use crate::home::PickerScreen;
use crate::keys::Key;
use crate::remote::Chip;
use contract::CommandId;
use contract::commands::hub::{Directories, HubCommand};

/// Whether the home chips' configuration is read: owed until one
/// read answers, failed until a new workspace or link owes it again.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) enum ChipRead {
    /// The next outgoing pass reads and folds the chips.
    #[default]
    Owed,
    /// One read answered: the chips are the hub's.
    Answered,
    /// The read failed: a notice says why, until a new workspace or
    /// link owes the next read.
    Failed,
}

/// The workspace chooser: the picked workspace, whether it is in git,
/// the new worktree switch and the picker.
#[derive(Default)]
pub(super) struct Chooser {
    /// The workspace picked for the next `start`, else the launch one.
    chosen: Option<String>,
    /// Whether the picked workspace is in git, read when it was
    /// chosen: the launch flag for the launch directory, else whether
    /// some row in that workspace was. Reading it back keeps the
    /// switch while its rows leave the feed.
    chosen_git: bool,
    /// The new worktree switch is on: the next `start` asks for a new
    /// worktree of the workspace. Choosing a workspace turns it off.
    worktree: bool,
    /// The workspace picker above the box, while open.
    picker: Option<Picker>,
    /// Whether the home chips' configuration is read.
    pub(super) chip_read: ChipRead,
    /// Whether the loop owes the pick the picker opens for.
    pub(super) start_pick: bool,
}

/// The workspace picker: the recent workspaces shown by the picker, the
/// selection while the recents show alone, and the typed path once a
/// key opens its row. The typed state stays for the opening, so a
/// failed listing is never asked again after Backspace to the recents
/// alone.
#[derive(Debug)]
pub(super) struct Picker {
    /// The recent workspaces, refreshed until a key touches the picker.
    recents: Vec<String>,
    /// The selected recent while the recents show alone.
    selected: usize,
    /// The typed path's state, kept for the opening.
    typed: Typed,
    /// No key has touched the picker since it opened: the recents
    /// below the typed row refill.
    untouched: bool,
}

impl Picker {
    /// Whether the typed row is open: a key opened it, and Backspace
    /// past its first character has not closed it. The row is open
    /// while its text holds a character.
    fn typing(&self) -> bool {
        !self.typed.text().is_empty()
    }

    /// The recent workspaces, refreshed until a key touches the picker.
    #[cfg(test)]
    pub(super) fn recents(&self) -> &[String] {
        &self.recents
    }

    /// The selected recent while the recents show alone.
    #[cfg(test)]
    pub(super) fn selected(&self) -> usize {
        self.selected
    }
}

impl Chooser {
    /// The workspace picked for the next `start`, if one was chosen.
    pub(super) fn chosen(&self) -> Option<&str> {
        self.chosen.as_deref()
    }

    /// Whether the new worktree switch is on.
    pub(super) fn worktree(&self) -> bool {
        self.worktree
    }

    /// The workspace picker above the box, while open.
    pub(super) fn picker(&self) -> Option<&Picker> {
        self.picker.as_ref()
    }
}

impl Home {
    /// No workspace is chosen: none was picked and there is no launch
    /// directory. An empty pick or launch directory counts as none.
    pub(in crate::app) fn no_workspace(&self) -> bool {
        self.chooser.chosen.as_deref().is_none_or(str::is_empty) && self.launch_dir().is_none()
    }

    /// The launch directory, when there is one: an empty launch path
    /// is none.
    fn launch_dir(&self) -> Option<String> {
        self.launch
            .location
            .workspace()
            .filter(|workspace| !workspace.as_os_str().is_empty())
            .map(|workspace| workspace.display().to_string())
    }

    /// The directory the picker's typed row starts at: the chip's
    /// workspace with a trailing `/`, else `~/` with no workspace chosen.
    fn picker_root(&self) -> String {
        match self.chooser.chosen.clone().or_else(|| self.launch_dir()) {
            Some(workspace) => format!("{}/", workspace.trim_end_matches('/')),
            None => "~/".to_owned(),
        }
    }

    /// The launch directory, when there is one, then distinct absolute
    /// row workspaces, at most ten.
    fn picker_recents(&self) -> Vec<String> {
        let mut list: Vec<String> = self.launch_dir().into_iter().collect();
        for row in self.sessions.all() {
            if list.len() == PICKER_CAP {
                break;
            }
            if !row.workspace.starts_with('/') {
                continue;
            }
            if !list.contains(&row.workspace) {
                list.push(row.workspace.clone());
            }
        }
        list
    }

    /// The picker follows arriving rows until a key touches it, and
    /// again once Backspace leaves the recents alone.
    pub(super) fn refill_picker(&mut self) {
        let Some(picker) = self.chooser.picker.as_ref() else {
            return;
        };
        if picker.typing() && !picker.untouched {
            return;
        }
        let focused = picker.recents.get(picker.selected).cloned();
        let recents = self.picker_recents();
        let selected = focused
            .and_then(|path| recents.iter().position(|row| *row == path))
            .unwrap_or(0);
        if let Some(picker) = self.chooser.picker.as_mut() {
            picker.recents = recents;
            picker.selected = selected;
        }
    }

    /// Whether the new worktree switch shows: the workspace in use is
    /// in git. With none chosen that is the launch directory; else the
    /// flag read when the workspace was chosen.
    pub(super) fn in_git(&self) -> bool {
        match &self.chooser.chosen {
            None => matches!(
                self.launch.location,
                crate::LaunchLocation::Local { git: true, .. }
            ),
            Some(_) => self.chooser.chosen_git,
        }
    }

    /// Records the workspace picked for the next `start`, turning the
    /// new worktree switch off and keeping whether it is in git: the
    /// launch flag for the launch directory, else whether some row in
    /// that workspace is then.
    pub(in crate::app) fn choose(&mut self, workspace: String) {
        self.chooser.chosen_git = match &self.launch.location {
            crate::LaunchLocation::Local {
                workspace: launch,
                git,
                ..
            } if workspace == launch.display().to_string() => *git,
            crate::LaunchLocation::Local { .. } | crate::LaunchLocation::Remote { .. } => {
                self.sessions.in_git(&workspace)
            }
        };
        self.chooser.chosen = Some(workspace);
        self.chooser.worktree = false;
        self.chooser.picker = None;
    }

    /// What the open picker draws: its recents, the focused row's index
    /// over the completions then the recents while typing, and the typed
    /// row with its completions.
    pub(super) fn picker_screen(&self) -> Option<PickerScreen> {
        let picker = self.chooser.picker.as_ref()?;
        let typing = picker.typing();
        let typed = typing.then(|| {
            let typed = &picker.typed;
            let completions: Vec<String> =
                typed.completions().into_iter().map(str::to_owned).collect();
            crate::home::TypedScreen {
                text: typed.text().to_owned(),
                completions,
                missing: typed.missing(),
            }
        });
        let focus = match (typing, &typed) {
            (false, _) => Some(picker.selected),
            (true, None) => None,
            (true, Some(drawn)) => match picker.typed.focus() {
                typed::Focus::Nothing => None,
                typed::Focus::Completion(name) => {
                    drawn.completions.iter().position(|next| next == name)
                }
                typed::Focus::Recent(index) => Some(drawn.completions.len().saturating_add(*index)),
            },
        };
        Some(PickerScreen {
            escapes: !self.no_workspace(),
            recents: picker.recents.clone(),
            focus,
            typed,
        })
    }
}

impl App {
    /// The picker's keys while it is open: typing, completing and
    /// choosing it, while Ctrl+C passes through and anything else is
    /// swallowed so the draft keeps nothing typed into the picker. A
    /// key that queues a directory sends its `directories` lines while
    /// the link is up.
    pub(in crate::app) fn picker_key(&mut self, key: &Key) -> Option<Effect> {
        match key {
            Key::CtrlC => return None,
            Key::Up => self.move_picker(false),
            Key::Down => self.move_picker(true),
            Key::Enter => return self.enter_picker(),
            Key::Esc => self.close_picker(),
            Key::Char(c) => {
                self.type_picker(*c);
                return Some(self.picker_send());
            }
            Key::Backspace => {
                self.backspace_picker();
                self.home.refill_picker();
                return Some(self.picker_send());
            }
            Key::Tab => {
                self.tab_picker();
                return Some(self.picker_send());
            }
            Key::CtrlO
            | Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::AltA
            | Key::BackTab
            | Key::F1
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlV
            | Key::CtrlL
            | Key::CtrlF
            | Key::AltUp
            | Key::AltDown
            | Key::ShiftUp
            | Key::ShiftDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_) => {}
        }
        Some(Effect::None)
    }

    /// Opens the workspace picker browsing: the typed row at the
    /// picker's root, asking the hub for its listing, with the recents
    /// below.
    pub(super) fn open_picker(&mut self) -> Effect {
        // A closed picker's asks are not this picker's: their answers
        // fall through as stale ids.
        self.outbox
            .settle_where(|waiting| matches!(waiting, Waiting::Directories(_)));
        let home = &mut self.home;
        let typed = Typed::at(&home.picker_root());
        home.chooser.picker = Some(Picker {
            recents: home.picker_recents(),
            selected: 0,
            typed,
            untouched: true,
        });
        self.picker_send()
    }

    /// Folds a hub answer for a `directories` line the open picker
    /// sent: `Some` with no lines to send when the id was the picker's,
    /// `None` when it was not, so other features' ids pass through
    /// untouched.
    pub(super) fn picker_answer(
        &mut self,
        id: &str,
        accepted: bool,
        payload: &Map<String, Value>,
    ) -> Option<Vec<String>> {
        let Some(Waiting::Directories(dir)) = self.outbox.waiting(id) else {
            return None;
        };
        let picker = self.home.chooser.picker.as_mut()?;
        if accepted {
            picker.typed.answered(&dir, payload.get("result"));
        } else {
            picker.typed.rejected(&dir);
        }
        self.outbox.settled(id);
        Some(Vec::new())
    }

    /// The open picker's queued `directories` lines, once the link is
    /// up: queued directories go out at the end of every `on_line`.
    /// Before them the home chips read once: the hub model, thinking
    /// and scope fold into the launch, and a failed read is a notice
    /// until a new workspace or link owes the next one.
    pub(super) fn picker_asks(&mut self) -> Vec<String> {
        if !self.outbox.is_up() {
            return Vec::new();
        }
        self.read_chips();
        let out = self.asked_directories();
        let home = &mut self.home;
        let Some(picker) = home.chooser.picker.as_mut() else {
            return Vec::new();
        };
        let outbox = &mut self.outbox;
        picker
            .typed
            .due(|dir| out.iter().any(|asked| asked == dir))
            .into_iter()
            .map(|dir| {
                let id = contract::CommandId(support::mint("c_"));
                outbox.wait(&id.0, Waiting::Directories(dir.clone()));
                let sent = directories_line(&id, &dir);
                // The picker's ask cruises the fold: its id rides
                // paired, so a failed write settles by the carried id.
                outbox.note_hub_sent(&sent.id, &sent.line);
                sent.line
            })
            .collect()
    }

    /// Reads the home chips' configuration when one is owed, folding
    /// it into the launch: the hub model and thinking on screen, and
    /// the scoped list the checklist marks. With no remote nothing is
    /// owed and nothing is read; a failed read is a notice. An answer
    /// names only what the hub configures: an absent model, level or
    /// list keeps the chip's text, and a flag-named key keeps its flag.
    fn read_chips(&mut self) {
        // `Failed` waits for a new workspace or link: only `Owed`
        // reads here.
        if !self.chip_read_owed() {
            return;
        }
        let owed = self.home.chooser.chip_read == ChipRead::Owed;
        if !owed {
            return;
        }
        let Some(remote) = self.remote() else {
            // A local home owes no chip read: there is no hub to ask.
            self.home.chooser.chip_read = ChipRead::Answered;
            return;
        };
        let read = match self.workspace() {
            Some(workspace) => remote.chip(&workspace),
            // With no local workspace the chips still read: the hub's
            // global layer stands in for the workspace.
            None => remote.chip_global(),
        };
        self.fold_chip(read);
    }

    /// Folds a chip read into the launch: the hub model and thinking
    /// on screen, and the scoped list the checklist marks. A flag
    /// names its key, so the layer's `model=` and thinking keys stand:
    /// the answer never replaces them. A rejection is one notice with
    /// the hub's code and message, and the chip keeps its text.
    fn fold_chip(&mut self, read: Result<Chip, crate::configure::ConfigureError>) {
        match read {
            Ok(chip) => {
                let thinking_key = chip
                    .model
                    .as_deref()
                    .map(super::super::model_picker::canonical_thinking_key);
                let overrides = self.launch_overrides.clone();
                let home = &mut self.home;
                if chip.model.is_some() && !Self::override_names("model", &overrides) {
                    home.launch.model = chip.model;
                }
                if chip.thinking.is_some()
                    && !Self::override_names("thinking", &overrides)
                    && thinking_key
                        .as_deref()
                        .is_none_or(|key| !Self::override_names(key, &overrides))
                {
                    home.launch.thinking = chip.thinking;
                }
                home.launch.scoped_models = chip.scoped.clone();
                home.chooser.chip_read = ChipRead::Answered;
                self.model_picker.adopt_scoped(chip.scoped);
            }
            Err(error) => {
                self.home.chooser.chip_read = ChipRead::Failed;
                self.push_notice(error.code.clone(), error.message);
            }
        }
    }

    /// Whether the launch layer names `key`: a flag's `key=value` or a
    /// bare `key` stands, so the chip read never replaces it.
    fn override_names(key: &str, overrides: &[String]) -> bool {
        overrides
            .iter()
            .any(|text| text.split_once('=').map_or(text.as_str(), |(key, _)| key) == key)
    }

    /// Whether the chips still need their read: owed, or failed and
    /// waiting for its re-owe. The read itself runs only in state
    /// `Owed`; `Failed` waits for a new workspace or link.
    pub(in crate::app) fn chip_read_owed(&self) -> bool {
        matches!(
            self.home.chooser.chip_read,
            ChipRead::Owed | ChipRead::Failed
        )
    }

    /// Whether the last chip read failed: the start pick stays owed
    /// until a new workspace or link reads the chips again.
    pub(in crate::app) fn chip_read_failed(&self) -> bool {
        self.home.chooser.chip_read == ChipRead::Failed
    }

    /// Owes the pick the picker opens for: the next outgoing pass
    /// reads it, opening on the answered catalogue.
    pub(crate) fn owe_start_pick(&mut self) {
        self.home.chooser.start_pick = true;
    }

    /// Takes the owed start pick, if one waits.
    pub(crate) fn take_start_pick(&mut self) -> bool {
        std::mem::replace(&mut self.home.chooser.start_pick, false)
    }

    /// The directories with a `directories` ask out, in send order.
    fn asked_directories(&self) -> Vec<String> {
        self.outbox
            .tags()
            .filter_map(|(_, waiting)| {
                if let Waiting::Directories(dir) = waiting {
                    Some(dir.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Forgets the picker's in-flight `directories` tags: with a typed
    /// text open their directories queue again, asked once more, in the
    /// order they went out, while with the recents alone open they drop.
    /// The cache stays either way.
    /// The chips read again on the next pass: the hub and its levels
    /// are gone, so the next link owes a fresh read.
    pub(in crate::app) fn picker_disconnected(&mut self) {
        let retry = self.asked_directories();
        self.outbox
            .settle_where(|waiting| matches!(waiting, Waiting::Directories(_)));
        self.home.chooser.chip_read = ChipRead::Owed;
        if let Some(picker) = self.home.chooser.picker.as_mut()
            && picker.typing()
        {
            picker.typed.disconnected(retry);
        }
    }

    /// Moves the picker's selection one row: through the completions,
    /// then the recents while typing, else wrapping through the recents.
    fn move_picker(&mut self, down: bool) {
        if let Some(picker) = self.home.chooser.picker.as_mut() {
            picker.untouched = false;
            if picker.typing() {
                let recents = picker.recents.len();
                picker.typed.move_focus(down, recents);
            } else {
                let len = picker.recents.len();
                if len > 0 {
                    picker.selected = if down {
                        (picker.selected + 1) % len
                    } else {
                        picker.selected.checked_sub(1).unwrap_or(len - 1)
                    };
                }
            }
        }
    }

    /// Types `c` into the picker's path, opening its row on the first
    /// key: the text is append-only.
    fn type_picker(&mut self, c: char) {
        if let Some(picker) = self.home.chooser.picker.as_mut() {
            picker.untouched = false;
            picker.typed.push(c);
        }
    }

    /// Removes the path's last character: past the first character the
    /// picker returns to the recents alone, keeping the listings for
    /// the opening. With no typed text it does nothing.
    fn backspace_picker(&mut self) {
        if let Some(picker) = self.home.chooser.picker.as_mut()
            && picker.typing()
        {
            picker.untouched = false;
            if !picker.typed.pop() {
                picker.selected = 0;
            }
        }
    }

    /// Puts the focused completion into the path with a trailing `/`.
    /// With a recent or nothing focused it changes nothing.
    fn tab_picker(&mut self) {
        if let Some(picker) = self.home.chooser.picker.as_mut()
            && picker.typing()
        {
            picker.untouched = false;
            picker.typed.tab();
        }
    }

    /// Sends the picker's queued directories while the link is up.
    fn picker_send(&mut self) -> Effect {
        let asks = self.picker_asks();
        if asks.is_empty() {
            Effect::None
        } else {
            Effect::Send(asks)
        }
    }

    /// Enters the picker's focused row as the workspace. With nothing
    /// focused and the typed directory listed, Enter resolves the typed
    /// text; while its listing is missing, or the link is down, Enter
    /// does nothing. With the recents alone open, Enter chooses as it
    /// always did, whatever the link. Enter never starts a `start`.
    fn enter_picker(&mut self) -> Option<Effect> {
        let typed_open = self
            .home
            .chooser
            .picker
            .as_ref()
            .is_some_and(|picker| picker.typing());
        if typed_open && !self.outbox.is_up() {
            return Some(Effect::None);
        }
        if !typed_open {
            self.choose_picker();
            return Some(Effect::None);
        }
        let choice = {
            let home = &mut self.home;
            let picker = home.chooser.picker.as_mut()?;
            let typed = &mut picker.typed;
            if let Some(target) = typed.completion_target() {
                Some(target)
            } else if let Some(index) = typed.recent() {
                picker.recents.get(index).cloned()
            } else {
                match typed.resolve() {
                    Resolved::Open(workspace) => Some(workspace),
                    Resolved::Missing | Resolved::Loading => None,
                }
            }
        };
        if let Some(workspace) = choice {
            self.choose_workspace(workspace);
        }
        // Bare `~` may have queued the home listing: send it while up.
        let asks = self.picker_asks();
        if asks.is_empty() {
            Some(Effect::None)
        } else {
            Some(Effect::Send(asks))
        }
    }

    /// Chooses `workspace` for the next `start` and names its project
    /// for `prompt_history`. An empty choice clears the pick: it is no
    /// workspace, so Enter reopens the picker. The chips belong to the
    /// old workspace, so with the link up the chips read at once at the
    /// new workspace, and with it down the next hello reads them.
    pub(in crate::app) fn choose_workspace(&mut self, workspace: String) {
        if workspace.is_empty() {
            self.home.chooser.chosen = None;
            self.home.chooser.worktree = false;
            self.home.chooser.picker = None;
        } else {
            self.home.choose(workspace);
        }
        if self.outbox.is_up()
            && let Some(remote) = self.remote()
        {
            let read = match self.workspace() {
                Some(workspace) => remote.chip(&workspace),
                None => remote.chip_global(),
            };
            self.fold_chip(read);
            self.sync_project();
            return;
        }
        self.home.chooser.chip_read = ChipRead::Owed;
        self.sync_project();
    }

    /// Chooses the picker's selected recent for the next `start`,
    /// turning the new worktree switch off.
    fn choose_picker(&mut self) {
        let choice = self
            .home
            .chooser
            .picker
            .as_ref()
            .and_then(|picker| picker.recents.get(picker.selected).cloned());
        if let Some(workspace) = choice {
            self.choose_workspace(workspace);
        }
    }

    /// Closes the picker, keeping the workspace as it was.
    fn close_picker(&mut self) {
        if !self.home.no_workspace() {
            self.home.chooser.picker = None;
        }
    }

    /// Clicks the picker row `at`: past the current recents it
    /// does nothing. Choosing a workspace turns the new worktree switch
    /// off.
    pub(super) fn pick(&mut self, at: usize) -> Effect {
        let choice = self
            .home
            .chooser
            .picker
            .as_ref()
            .and_then(|picker| picker.recents.get(at).cloned());
        if let Some(workspace) = choice {
            self.choose_workspace(workspace);
        }
        Effect::None
    }

    /// Flips the new worktree switch.
    pub(super) fn toggle_worktree(&mut self) -> Effect {
        self.home.chooser.worktree = !self.home.chooser.worktree;
        Effect::None
    }

    /// The workspace in use on home: the attached session's row
    /// workspace, else the picked one, else the launch workspace, else
    /// none. An empty value at each step is none, so `None` holds exactly
    /// when no non-empty row, pick, or launch directory names one.
    pub(in crate::app) fn home_workspace(&self) -> Option<PathBuf> {
        let home = &self.home;
        if let Some(session) = self.session()
            && let Some(row) = home.sessions.row(session)
            && let Some(workspace) = non_empty(PathBuf::from(&row.workspace))
        {
            return Some(workspace);
        }
        if let Some(chosen) = home.chooser.chosen()
            && let Some(workspace) = non_empty(PathBuf::from(chosen))
        {
            return Some(workspace);
        }
        let launch = home.launch.location.workspace()?;
        non_empty(launch.to_path_buf())
    }
}

/// A workspace path with empty filtered out: an empty launch, row, or
/// chosen value is no workspace.
fn non_empty(path: PathBuf) -> Option<PathBuf> {
    (!path.as_os_str().is_empty()).then_some(path)
}

/// A `directories` line for `dir` with its command id carried
/// alongside: no `session_id`, as a session's commands carry.
fn directories_line(id: &CommandId, dir: &str) -> Sent {
    Sent {
        line: hub_line(
            id,
            HubCommand::Directories(Directories {
                path: dir.to_owned(),
            }),
        ),
        id: id.clone(),
    }
}

/// The picker's rows: the launch directory and nine row workspaces.
const PICKER_CAP: usize = 10;

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
