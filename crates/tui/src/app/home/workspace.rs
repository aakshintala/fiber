//! Home's workspace chooser: the workspace picked for the next `start`,
//! whether it is in git, the new worktree switch and the workspace
//! picker above the box.

use std::path::PathBuf;

use super::Home;
use super::super::{App, Effect};
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
    /// The workspace picker above the box: its list, fixed at open, and
    /// the selected index.
    picker: Option<(Vec<String>, usize)>,
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
    pub(super) fn picker(&self) -> Option<&(Vec<String>, usize)> {
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
}

impl App {
    /// The picker's keys while it is open: moving, choosing and closing
    /// it, while Ctrl+C passes through and anything else is swallowed
    /// so the draft keeps nothing typed into the picker.
    pub(super) fn picker_key(&mut self, key: &Key) -> Option<Effect> {
        match key {
            Key::CtrlC => return None,
            Key::Up => self.move_picker(false),
            Key::Down => self.move_picker(true),
            Key::Enter => self.choose_picker(),
            Key::Esc => self.close_picker(),
            Key::Char(_)
            | Key::Backspace
            | Key::CtrlO
            | Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::AltA
            | Key::Tab
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
    /// first, then each distinct workspace from the feed rows and then
    /// the recent rows, at most ten. Its list is fixed at open.
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
            if !list.contains(&row.workspace) {
                list.push(row.workspace.clone());
            }
        }
        home.chooser.picker = Some((list, 0));
        Effect::None
    }

    /// Moves the picker's selection one row, clamped to its list.
    fn move_picker(&mut self, down: bool) {
        if let Some(home) = self.home.as_mut()
            && let Some((list, selected)) = home.chooser.picker.as_mut()
        {
            *selected = if down {
                (*selected)
                    .saturating_add(1)
                    .min(list.len().saturating_sub(1))
            } else {
                (*selected).saturating_sub(1)
            };
        }
    }

    /// Chooses the picker's selected workspace for the next `start`,
    /// turning the new worktree switch off.
    fn choose_picker(&mut self) {
        let choice = self
            .home
            .as_ref()
            .and_then(|home| home.chooser.picker.as_ref())
            .and_then(|(list, selected)| list.get(*selected).cloned());
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

    /// Clicks the picker row `at`: past the list fixed at open it does
    /// nothing. Choosing a workspace turns the new worktree switch off.
    pub(super) fn pick(&mut self, at: usize) -> Effect {
        let choice = self
            .home
            .as_ref()
            .and_then(|home| home.chooser.picker.as_ref())
            .and_then(|(list, _)| list.get(at).cloned());
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

/// The picker's rows: the launch directory and nine row workspaces.
const PICKER_CAP: usize = 10;
