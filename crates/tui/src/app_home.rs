//! Home's state on the app: the launch description, whether a `start`
//! went out, and what home draws (`docs/tui.md`, "Home"). The data it
//! draws lives in [`crate::home`]; this module is `App`'s home.

use serde_json::{Value, json};

use super::{App, Link, mint};
use crate::home::{HomeScreen, Launch, Left, Sessions, from_status, line, recent_rows};
use crate::link::Line;
use contract::SessionId;

/// Home's state: the launch description, and whether a `start` went out in
/// this run, which hides the input box's placeholder.
pub(super) struct Home {
    /// What the terminal knows about where it was launched.
    launch: Launch,
    /// A `start` went out in this run.
    prompted: bool,
    /// The session list: live rows from the feed, exited rows from `recent`.
    sessions: Sessions,
    /// `feed` and the first `recent` page went out.
    fed: bool,
    /// The `feed` command waiting for its answer.
    feed_id: Option<String>,
    /// The latest `recent` command waiting for its answer, and whether it
    /// asked the first page.
    recent_ask: Option<(String, bool)>,
}

impl App {
    /// Stores the launch description as home, keying prompt history by its
    /// project as the run argument did. The app keeps today's screen, so
    /// the jigs and every existing app test stay byte-identical.
    /// Stores the launch description as home, keying prompt history by its
    /// project as the run argument did. The app keeps today's screen, so
    /// the jigs and every existing app test stay byte-identical.
    pub(crate) fn set_home(&mut self, launch: Launch) {
        self.set_project(launch.project.clone());
        self.home = Some(Home {
            launch,
            prompted: false,
            sessions: Sessions::default(),
            fed: false,
            feed_id: None,
            recent_ask: None,
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
            rows: home
                .sessions
                .shown(&home.launch.project, false)
                .iter()
                .map(|row| (row.key, line(row, &home.launch.project), false))
                .collect(),
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

    /// The hub lines home asks for, once the link is up: `feed`, then the
    /// first `recent` page. The session list fills in when the hub's feed
    /// arrives.
    pub(super) fn home_outgoing(&mut self) -> Vec<String> {
        if self.link != Link::Up {
            return Vec::new();
        }
        let Some(home) = self.home.as_mut() else {
            return Vec::new();
        };
        if home.fed {
            return Vec::new();
        }
        home.fed = true;
        let feed = mint();
        let recent = mint();
        home.feed_id = Some(feed.clone());
        home.recent_ask = Some((recent.clone(), true));
        vec![
            json!({"id": feed, "command": "feed"}).to_string(),
            json!({"id": recent, "command": "recent"}).to_string(),
        ]
    }

    /// Folds one hub line into the session list, with the lines to send;
    /// `None` means `on_line` goes on as today. A hub `session_left` and
    /// every `session_status` fold into the rows; the answers to `feed`
    /// and the latest `recent` fill the list, and their refusals are one
    /// notice. `attention` and other hub lines pass through untouched, and
    /// a status still reaches the conversation below.
    pub(super) fn home_line(&mut self, line: &Line) -> Option<Vec<String>> {
        self.home.as_ref()?;
        match line {
            Line::Hub(hub) => match hub.kind.as_str() {
                "session_left" => {
                    let id = hub.payload.get("session_id").and_then(Value::as_str)?;
                    let how = match hub.payload.get("how").and_then(Value::as_str) {
                        Some("exited") => Left::Exited,
                        Some("crashed") => Left::Crashed,
                        _ => return Some(Vec::new()),
                    };
                    if let Some(home) = self.home.as_mut() {
                        home.sessions.left(&SessionId(id.to_owned()), how);
                    }
                    Some(Vec::new())
                }
                "command_accepted" | "command_rejected" => {
                    let id = hub.payload.get("command_id").and_then(Value::as_str)?;
                    let accepted = hub.kind.as_str() == "command_accepted";
                    let is_feed =
                        self.home.as_ref().and_then(|home| home.feed_id.as_deref()) == Some(id);
                    if is_feed {
                        if let Some(home) = self.home.as_mut() {
                            home.feed_id = None;
                        }
                        if !accepted {
                            self.notices.push(refusal(&hub.payload));
                        }
                        return Some(Vec::new());
                    }
                    let recent = self
                        .home
                        .as_ref()
                        .and_then(|home| home.recent_ask.as_ref())
                        .filter(|(asked, _)| asked == id)
                        .map(|(_, first)| *first);
                    if let Some(first) = recent {
                        if let Some(home) = self.home.as_mut() {
                            home.recent_ask = None;
                        }
                        if accepted {
                            let rows = hub
                                .payload
                                .get("result")
                                .map(recent_rows)
                                .unwrap_or_default();
                            if let Some(home) = self.home.as_mut() {
                                home.sessions.recent(rows, first);
                            }
                        } else {
                            self.notices.push(refusal(&hub.payload));
                        }
                        return Some(Vec::new());
                    }
                    None
                }
                _ => None,
            },
            Line::Session(envelope) => {
                if envelope.kind == "session_status" {
                    let row = from_status(envelope);
                    if let Some(home) = self.home.as_mut() {
                        home.sessions.status(row);
                    }
                }
                None
            }
        }
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

/// A hub refusal's message, for a notice.
fn refusal(payload: &serde_json::Map<String, Value>) -> String {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("rejected")
        .to_owned()
}

#[cfg(test)]
#[path = "app_home_tests.rs"]
mod tests;
