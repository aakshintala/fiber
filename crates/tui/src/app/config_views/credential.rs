//! The credential picker and its admitted-provider save (`docs/tui.md`,
//! "Swapped views"). Queries and switches belong to the session; only an
//! accepted switch can write the global label.

use super::{App, Effect, Open};
use crate::app::{Kind, mint, session_command};
use crate::input::Draft;
use crate::keys::Key;
use crate::swapped::{Frame, Ink, List, Spot, rows_height};
use crate::theme::Role;
use contract::events::{CommandAccepted, CommandRejected, CommandResult, CredentialLabels};
use contract::{Envelope, SessionId};
use serde_json::json;

/// The labels query, its answer and the selection over that answer.
#[derive(Debug)]
pub(crate) struct Picker {
    state: State,
    list: List,
}

#[derive(Debug)]
enum State {
    Waiting(String),
    Answered(CredentialLabels),
    Rejected(String),
}

/// What a picker input asks the app to do.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    /// Keep the picker open.
    Stay,
    /// Close without switching.
    Close,
    /// Switch to this label, saving unless this session only.
    Choose { label: String, session_only: bool },
}

impl Picker {
    /// Opens while the query with `id` waits.
    pub(crate) fn open(id: String) -> Self {
        Self {
            state: State::Waiting(id),
            list: List::default(),
        }
    }

    /// Fills only the outstanding query, selecting its current label.
    pub(crate) fn answered(&mut self, id: &str, labels: CredentialLabels, height: usize) {
        if !matches!(&self.state, State::Waiting(waiting) if waiting == id) {
            return;
        }
        let selected = labels
            .credentials
            .iter()
            .position(|label| Some(label) == labels.credential.as_ref())
            .unwrap_or(0);
        let count = labels.credentials.len();
        self.state = State::Answered(labels);
        self.list
            .select(selected, count, rows_height(&self.frame(), height));
    }

    /// Shows only the outstanding query's rejection.
    pub(crate) fn rejected(&mut self, id: &str, message: &str) {
        if !matches!(&self.state, State::Waiting(waiting) if waiting == id) {
            return;
        }
        self.state = State::Rejected(message.to_owned());
    }

    /// Moves, chooses or closes; typing never filters.
    pub(crate) fn key(&mut self, key: &Key, height: usize) -> Pick {
        match key {
            Key::Esc => Pick::Close,
            Key::Enter => self.choose(false),
            Key::Char(_)
            | Key::Backspace
            | Key::CtrlC
            | Key::CtrlO
            | Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::Up
            | Key::Down
            | Key::AltA
            | Key::Tab
            | Key::BackTab
            | Key::F1
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlF
            | Key::AltUp
            | Key::AltDown
            | Key::AltX
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_)
            | Key::CtrlV
            | Key::CtrlL => {
                let count = self.answer().map_or(0, |answer| answer.credentials.len());
                self.list
                    .key(key, count, rows_height(&self.frame(), height));
                Pick::Stay
            }
        }
    }

    /// A row click chooses and saves; the header closes.
    pub(crate) fn click(&mut self, spot: Spot, height: usize) -> Pick {
        match spot {
            Spot::Close => Pick::Close,
            Spot::Row(at) => {
                let Some(answer) = self.answer() else {
                    return Pick::Stay;
                };
                let Some(label) = answer.credentials.get(at) else {
                    return Pick::Stay;
                };
                let pick = Pick::Choose {
                    label: label.clone(),
                    session_only: false,
                };
                self.list.select(
                    at,
                    answer.credentials.len(),
                    rows_height(&self.frame(), height),
                );
                pick
            }
            Spot::Cell(..) | Spot::Switch { .. } | Spot::Revoke(_) | Spot::Item(_) => Pick::Stay,
        }
    }

    /// Chooses without changing the default.
    pub(crate) fn session_only(&self) -> Pick {
        self.choose(true)
    }

    fn choose(&self, session_only: bool) -> Pick {
        self.answer()
            .and_then(|answer| answer.credentials.get(self.list.selected()))
            .map_or(Pick::Stay, |label| Pick::Choose {
                label: label.clone(),
                session_only,
            })
    }

    fn answer(&self) -> Option<&CredentialLabels> {
        match &self.state {
            State::Answered(answer) => Some(answer),
            State::Waiting(_) | State::Rejected(_) => None,
        }
    }

    /// Draws in the shared swapped-view frame.
    pub(crate) fn frame(&self) -> Frame {
        let rows = self.answer().map_or_else(Vec::new, |answer| {
            answer
                .credentials
                .iter()
                .map(|label| {
                    let mut cells = vec![(label.clone(), None, Ink::Plain)];
                    if Some(label) == answer.credential.as_ref() {
                        cells.push(("  ● current".to_owned(), None, Ink::Role(Role::Accent)));
                    }
                    cells
                })
                .collect()
        });
        let below = match &self.state {
            State::Rejected(message) => message.clone(),
            State::Answered(answer) => {
                if answer.credentials.is_empty() {
                    format!("{} has no credential labels", answer.provider)
                } else {
                    answer.provider.clone()
                }
            }
            State::Waiting(_) => "Reading the labels…".to_owned(),
        };
        Frame {
            title: "Credentials".to_owned(),
            command: "/credential",
            rows,
            list: self.list,
            below: vec![below],
            field: None,
            footer: "↑↓ move · Enter choose · Ctrl+S this session · Esc close".to_owned(),
        }
    }
}

const NO_SAVE: &str = "Saving is not available; the switch is for this session only.";

/// Only the latest query and latest saving switch wait for answers.
#[derive(Default)]
pub(in crate::app) struct Waits {
    ask: Option<Ask>,
    save: Option<Save>,
}

impl Waits {
    fn take<T>(slot: &mut Option<T>, id: &str, key: fn(&T) -> &str) -> Option<T> {
        if slot.as_ref().map(key) == Some(id) {
            slot.take()
        } else {
            None
        }
    }
}

struct Ask {
    id: String,
    session: SessionId,
    label: String,
}
struct Save {
    id: String,
    label: String,
}

impl App {
    /// Opens the picker or queries before switching to the trimmed label.
    pub(in crate::app) fn credential_command(&mut self, rest: &str) -> Effect {
        let label = rest.trim();
        if label.is_empty() {
            self.config_views.credential.ask = None;
            return self.open_config_view(super::ConfigView::Credential);
        }
        let Some((session, _)) = self.command_session() else {
            return Effect::None;
        };
        let id = mint();
        let line = session_command(&id, "credentials", &session, None).to_string();
        self.outbox
            .pending
            .insert(id.clone(), (Kind::Command, std::mem::take(&mut self.draft)));
        self.config_views.credential.ask = Some(Ask {
            id,
            session,
            label: label.to_owned(),
        });
        Effect::Send(vec![line])
    }

    /// Whether the credential picker is the open configuration view.
    pub(in crate::app) fn credential_view_open(&self) -> bool {
        matches!(self.config_views.open, Some(Open::Credential(_)))
    }

    /// Chooses for this session; the caller runs it only while the picker
    /// is the surface on top.
    pub(in crate::app) fn credential_session_only(&mut self) -> Effect {
        let Some(Open::Credential(picker)) = &self.config_views.open else {
            return Effect::None;
        };
        self.credential_pick(picker.session_only())
    }

    pub(super) fn credential_pick(&mut self, pick: Pick) -> Effect {
        match pick {
            Pick::Stay => Effect::None,
            Pick::Close => {
                self.config_views.open = None;
                Effect::None
            }
            Pick::Choose {
                label,
                session_only,
            } => {
                let Some(Open::Credential(picker)) = &self.config_views.open else {
                    return Effect::None;
                };
                let Some(labels) = picker.answer().cloned() else {
                    return Effect::None;
                };
                let send = self.credential_switch(&labels, label, session_only);
                if send.is_empty() {
                    Effect::None
                } else {
                    self.config_views.open = None;
                    Effect::Send(send)
                }
            }
        }
    }

    fn credential_switch(
        &mut self,
        labels: &CredentialLabels,
        label: String,
        session_only: bool,
    ) -> Vec<String> {
        if !self.outbox.is_up() {
            self.push_notice(
                "connection_failed",
                "The hub is not connected; nothing changed.".to_owned(),
            );
            return Vec::new();
        }
        let Some(session) = self.session().cloned() else {
            return Vec::new();
        };
        self.say_cache_rebuild(Some(&label) != labels.credential.as_ref());
        let id = mint();
        let line =
            session_command(&id, "credential", &session, Some(json!({"label":label}))).to_string();
        self.outbox
            .pending
            .insert(id.clone(), (Kind::Command, Draft::default()));
        self.config_views.credential.save = None;
        if !session_only {
            if self.configure_seam().is_some() {
                self.config_views.credential.save = Some(Save { id, label });
            } else {
                self.push_notice("io_failed", NO_SAVE.to_owned());
            }
        }
        vec![line]
    }

    fn say_cache_rebuild(&mut self, changed: bool) {
        if changed && let Some(tokens) = self.usage_on_screen() {
            self.push_notice(
                "cache_rebuild",
                format!(
                    "switching rebuilds the cache: about {} tokens",
                    crate::swapped::about(tokens)
                ),
            );
        }
    }

    /// Saves only the newest switch under the provider that admitted it.
    pub(in crate::app) fn credential_accepted(&mut self, accepted: &CommandAccepted) {
        let Some(save) = Waits::take(
            &mut self.config_views.credential.save,
            &accepted.command_id.0,
            |save| &save.id,
        ) else {
            return;
        };
        let Some(CommandResult::CredentialSwitch { provider }) = &accepted.result else {
            self.push_notice("io_failed", NO_SAVE.to_owned());
            return;
        };
        let key = format!("providers.\"{provider}\".credential");
        match self.write_global(&key, &save.label) {
            Some(Ok(saved)) => {
                for warning in saved.warnings {
                    self.push_notice("config_invalid", warning);
                }
            }
            Some(Err(error)) => self.push_notice(
                "io_failed",
                format!("Saving {key} failed: {}", error.message),
            ),
            None => self.push_notice("io_failed", NO_SAVE.to_owned()),
        }
    }

    pub(super) fn credential_line(&mut self, envelope: &Envelope) -> Vec<String> {
        match envelope.kind.as_str() {
            "command_accepted" => {
                let Some(accepted) = crate::app::read!(envelope, CommandAccepted) else {
                    return Vec::new();
                };
                let Some(CommandResult::Credentials(labels)) = accepted.result else {
                    return Vec::new();
                };
                let height = self.config_view_height();
                if let Some(Open::Credential(picker)) = &mut self.config_views.open {
                    picker.answered(&accepted.command_id.0, labels.clone(), height);
                }
                if let Some(ask) = Waits::take(
                    &mut self.config_views.credential.ask,
                    &accepted.command_id.0,
                    |ask| &ask.id,
                ) && self.session() == Some(&ask.session)
                {
                    return self.credential_switch(&labels, ask.label, false);
                }
            }
            "command_rejected" => {
                let Some(rejected) = crate::app::read!(envelope, CommandRejected) else {
                    return Vec::new();
                };
                let Some(id) = rejected.command_id else {
                    return Vec::new();
                };
                Waits::take(&mut self.config_views.credential.save, &id.0, |save| {
                    &save.id
                });
                Waits::take(&mut self.config_views.credential.ask, &id.0, |ask| &ask.id);
                if let Some(Open::Credential(picker)) = &mut self.config_views.open {
                    picker.rejected(&id.0, &rejected.message);
                }
            }
            _ => {}
        }
        Vec::new()
    }
}

#[cfg(test)]
#[path = "credential_tests.rs"]
mod tests;
