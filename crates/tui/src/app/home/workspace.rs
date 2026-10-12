//! Home's workspace chooser: the workspace picked for the next `start`,
//! whether it is in git, the new worktree switch and the workspace
//! picker above the box.

use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::super::{App, Effect, Link, mint};
use super::typed::{Resolved, Typed};
use super::{Home, typed};
use crate::home::PickerScreen;
use crate::keys::Key;

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
}

/// The workspace picker: the recent workspaces fixed at open, the
/// selection while the recents show alone, and the typed path once a
/// key opens its row. The typed state stays for the opening, so a
/// failed listing is never asked again after Backspace to the recents
/// alone.
#[derive(Debug)]
pub(super) struct Picker {
    /// The recent workspaces, fixed at open.
    recents: Vec<String>,
    /// The selected recent while the recents show alone.
    selected: usize,
    /// The typed path's state, kept for the opening.
    typed: Typed,
    /// The typed row is open: a key opened it, and Backspace past its
    /// first character has not closed it.
    typing: bool,
}

impl Default for Picker {
    fn default() -> Self {
        Self {
            recents: Vec::new(),
            selected: 0,
            typed: Typed::new(),
            typing: false,
        }
    }
}

impl Picker {
    /// The recent workspaces, fixed at open.
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
    /// Whether the new worktree switch shows: the workspace in use is
    /// in git. With none chosen that is the launch directory; else the
    /// flag read when the workspace was chosen.
    pub(super) fn in_git(&self) -> bool {
        match &self.chooser.chosen {
            None => self.launch.git,
            Some(_) => self.chooser.chosen_git,
        }
    }

    /// Records the workspace picked for the next `start`, turning the
    /// new worktree switch off and keeping whether it is in git: the
    /// launch flag for the launch directory, else whether some row in
    /// that workspace is then.
    pub(in crate::app) fn choose(&mut self, workspace: String) {
        self.chooser.chosen_git = if workspace == self.launch.workspace.display().to_string() {
            self.launch.git
        } else {
            self.sessions.in_git(&workspace)
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
        let typing = picker.typing;
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
    pub(super) fn picker_key(&mut self, key: &Key) -> Option<Effect> {
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
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_) => {}
        }
        Some(Effect::None)
    }

    /// Opens the workspace picker above the box: the launch directory
    /// first, then each distinct absolute workspace from the feed rows
    /// and then the recent rows, at most ten. Its list is fixed at open.
    pub(super) fn open_picker(&mut self) -> Effect {
        let Some(home) = self.home.as_mut() else {
            return Effect::None;
        };
        let project = home.launch.project.clone();
        let mut list = vec![home.launch.workspace.display().to_string()];
        for row in home.sessions.shown(&project, false) {
            if list.len() == PICKER_CAP {
                break;
            }
            if row.workspace.is_empty() || !row.workspace.starts_with('/') {
                continue;
            }
            if !list.contains(&row.workspace) {
                list.push(row.workspace.clone());
            }
        }
        home.chooser.picker = Some(Picker {
            recents: list,
            selected: 0,
            typed: Typed::new(),
            typing: false,
        });
        Effect::None
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
        let home = self.home.as_mut()?;
        let picker = home.chooser.picker.as_mut()?;
        if accepted {
            picker
                .typed
                .answered(id, payload.get("result"))
                .then(Vec::new)
        } else {
            picker.typed.rejected(id).then(Vec::new)
        }
    }

    /// The open picker's queued `directories` lines, once the link is
    /// up: queued directories go out at the end of every `on_line`.
    pub(super) fn picker_asks(&mut self) -> Vec<String> {
        if self.link != Link::Up {
            return Vec::new();
        }
        let Some(home) = self.home.as_mut() else {
            return Vec::new();
        };
        let Some(picker) = home.chooser.picker.as_mut() else {
            return Vec::new();
        };
        picker
            .typed
            .due(mint)
            .into_iter()
            .map(|(id, dir)| directories_line(&id, &dir))
            .collect()
    }

    /// Forgets the picker's in-flight `directories` ids: with a typed
    /// text open their directories queue again, asked once more, while
    /// with the recents alone open they drop. The cache stays either way.
    pub(in crate::app) fn picker_disconnected(&mut self) {
        if let Some(home) = self.home.as_mut()
            && let Some(picker) = home.chooser.picker.as_mut()
        {
            if picker.typing {
                picker.typed.disconnected();
            } else {
                picker.typed.forget();
            }
        }
    }

    /// Moves the picker's selection one row: through the completions,
    /// then the recents while typing, else clamped to the recents.
    fn move_picker(&mut self, down: bool) {
        if let Some(home) = self.home.as_mut()
            && let Some(picker) = home.chooser.picker.as_mut()
        {
            if picker.typing {
                let recents = picker.recents.len();
                picker.typed.move_focus(down, recents);
            } else {
                picker.selected = if down {
                    picker
                        .selected
                        .saturating_add(1)
                        .min(picker.recents.len().saturating_sub(1))
                } else {
                    picker.selected.saturating_sub(1)
                };
            }
        }
    }

    /// Types `c` into the picker's path, opening its row on the first
    /// key: the text is append-only.
    fn type_picker(&mut self, c: char) {
        if let Some(home) = self.home.as_mut()
            && let Some(picker) = home.chooser.picker.as_mut()
        {
            picker.typing = true;
            picker.typed.push(c);
        }
    }

    /// Removes the path's last character: past the first character the
    /// picker returns to the recents alone, keeping the listings for
    /// the opening. With no typed text it does nothing.
    fn backspace_picker(&mut self) {
        if let Some(home) = self.home.as_mut()
            && let Some(picker) = home.chooser.picker.as_mut()
            && picker.typing
            && !picker.typed.pop()
        {
            picker.typing = false;
            picker.selected = 0;
        }
    }

    /// Puts the focused completion into the path with a trailing `/`.
    /// With a recent or nothing focused it changes nothing.
    fn tab_picker(&mut self) {
        if let Some(home) = self.home.as_mut()
            && let Some(picker) = home.chooser.picker.as_mut()
            && picker.typing
        {
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
        let typed_open = self.home.as_ref().is_some_and(|home| {
            home.chooser
                .picker
                .as_ref()
                .is_some_and(|picker| picker.typing)
        });
        if typed_open && self.link != Link::Up {
            return Some(Effect::None);
        }
        if !typed_open {
            self.choose_picker();
            return Some(Effect::None);
        }
        let choice = {
            let home = self.home.as_mut()?;
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
        if let Some(workspace) = choice
            && let Some(home) = self.home.as_mut()
        {
            home.choose(workspace);
        }
        // Bare `~` may have queued the home listing: send it while up.
        let asks = self.picker_asks();
        if asks.is_empty() {
            Some(Effect::None)
        } else {
            Some(Effect::Send(asks))
        }
    }

    /// Chooses the picker's selected recent for the next `start`,
    /// turning the new worktree switch off.
    fn choose_picker(&mut self) {
        let choice = self
            .home
            .as_ref()
            .and_then(|home| home.chooser.picker.as_ref())
            .and_then(|picker| picker.recents.get(picker.selected).cloned());
        if let (Some(home), Some(workspace)) = (self.home.as_mut(), choice) {
            home.choose(workspace);
        }
    }

    /// Closes the picker, keeping the workspace as it was.
    fn close_picker(&mut self) {
        if let Some(home) = self.home.as_mut() {
            home.chooser.picker = None;
        }
    }

    /// Clicks the picker row `at`: past the recents fixed at open it
    /// does nothing. Choosing a workspace turns the new worktree switch
    /// off.
    pub(super) fn pick(&mut self, at: usize) -> Effect {
        let choice = self
            .home
            .as_ref()
            .and_then(|home| home.chooser.picker.as_ref())
            .and_then(|picker| picker.recents.get(at).cloned());
        if let (Some(home), Some(workspace)) = (self.home.as_mut(), choice) {
            home.choose(workspace);
        }
        Effect::None
    }

    /// Flips the new worktree switch.
    pub(super) fn toggle_worktree(&mut self) -> Effect {
        if let Some(home) = self.home.as_mut() {
            home.chooser.worktree = !home.chooser.worktree;
        }
        Effect::None
    }

    /// The workspace in use on home: the attached session's row
    /// workspace, else the picked one, else the launch workspace.
    pub(in crate::app) fn home_workspace(&self) -> PathBuf {
        if let Some(home) = &self.home {
            if let Some(session) = self.session()
                && let Some(row) = home.sessions.row(session)
            {
                return PathBuf::from(&row.workspace);
            }
            if let Some(chosen) = &home.chooser.chosen {
                return PathBuf::from(chosen);
            }
            return home.launch.workspace.clone();
        }
        self.workspace.clone()
    }
}

/// A `directories` line for `dir` under `id`: no `session_id`, as a
/// session's commands carry.
fn directories_line(id: &str, dir: &str) -> String {
    json!({"id": id, "command": "directories", "args": {"path": dir}}).to_string()
}

/// The picker's rows: the launch directory and nine row workspaces.
const PICKER_CAP: usize = 10;

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
