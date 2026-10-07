//! Home's state on the app: the launch description, whether a `start`
//! went out, and what home draws (`docs/tui.md`, "Home"). The data it
//! draws lives in [`crate::home`]; this module is `App`'s home.

use serde_json::{Value, json};

use super::App;
use crate::home::{HomeScreen, Launch};

/// Home's state: the launch description, and whether a `start` went out in
/// this run, which hides the input box's placeholder.
pub(super) struct Home {
    /// What the terminal knows about where it was launched.
    launch: Launch,
    /// A `start` went out in this run.
    prompted: bool,
}

impl App {
    /// Stores the launch description as home, keying prompt history by its
    /// project as the run argument did. The app keeps today's screen, so
    /// the jigs and every existing app test stay byte-identical.
    pub(crate) fn set_home(&mut self, launch: Launch) {
        self.set_project(launch.project.clone());
        self.home = Some(Home {
            launch,
            prompted: false,
        });
    }

    /// Whether home draws: home is set, no session is attached, and neither
    /// the key map nor the approval panel covers the screen.
    pub(crate) fn on_home(&self) -> bool {
        self.home.is_some()
            && self.session().is_none()
            && self.keymap_top().is_none()
            && self.panel().is_none()
    }

    /// What home draws, or `None` unless [`App::on_home`] holds.
    pub(crate) fn home_screen(&self) -> Option<HomeScreen> {
        let home = self.home.as_ref().filter(|_| self.on_home())?;
        let segment = home
            .launch
            .workspace
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| home.launch.workspace.display().to_string());
        Some(HomeScreen {
            version: home.launch.version.clone(),
            // A remote client has no launch directory; the picker it would
            // always show is a later ticket's.
            glyph: "⌇".to_owned(),
            chips: vec![format!("[{segment}]"), "enter starts a session".to_owned()],
            foot: if self.armed_at.is_some() {
                super::QUIT_HINT.to_owned()
            } else {
                "↓ the session list · F1 the key map · Ctrl+C twice to quit".to_owned()
            },
            // Until the first prompt, the placeholder says "/? for
            // shortcuts"; no session exists before Enter, so opening the
            // terminal, glancing at home and leaving creates nothing.
            placeholder: self.draft.is_empty() && !home.prompted,
        })
    }

    /// The `start` args for `content`: the launch workspace on home, else
    /// the launch directory as today. Sending one hides the placeholder
    /// until the run ends.
    pub(super) fn start_args(&mut self, content: Value) -> Value {
        match &mut self.home {
            Some(home) => {
                home.prompted = true;
                json!({
                    "workspace": home.launch.workspace.display().to_string(),
                    "content": content,
                })
            }
            None => json!({
                "workspace": self.workspace.display().to_string(),
                "content": content,
            }),
        }
    }
}

#[cfg(test)]
#[path = "app_home_tests.rs"]
mod tests;
