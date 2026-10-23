//! The picker's not-installed provider rows (`docs/tui.md`, "Swapped
//! views"): which providers install, the one install that runs, the
//! failures and outcomes the open picker draws, and which row has the
//! focus. Choosing a row is the approval, so nothing here asks.

use std::path::PathBuf;
use std::sync::Arc;

use super::{Mode, ModelPicker, RowAt, filter, shown_in};
use crate::catalogue::Refresh;
use crate::configure::{Configure, ConfigureError, LoginNeed, ProviderInstall};
use crate::text::controls_to_space;

/// The job a start holds: the provider to install, where, and the seam
/// that installs it. The loop runs it off its own thread.
pub(crate) struct InstallStart {
    /// The provider to install, as the catalogue listed it.
    pub(crate) provider: String,
    /// The workspace the install addresses.
    pub(crate) workspace: PathBuf,
    /// The seam the install runs through.
    pub(crate) seam: Arc<dyn Configure>,
}

/// Where a provider's install stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Progress {
    /// The install runs now.
    Installing,
    /// The install failed: the error's first line, control characters
    /// turned to spaces.
    Failed(String),
}

/// What a landed install shows under the list until the picker closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    /// The provider chosen, control characters turned to spaces.
    pub(crate) provider: String,
    /// The install summary, one line per element, likewise cleaned.
    pub(crate) summary: Vec<String>,
    /// The providers needing a key, likewise cleaned.
    pub(crate) login: Vec<LoginNeed>,
}

/// What a result did to the install state.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Landed {
    /// The install worked. The notices carry the credential lines when
    /// the picker did not show them; empty when an outcome was stored.
    Installed {
        /// Each notice's code and text.
        notices: Vec<(&'static str, String)>,
    },
    /// The install failed with the picker not showing the rows: the
    /// notice text.
    Notice(String),
    /// Nothing to tell the loop: no install of that provider was running,
    /// or the open picker shows the failure on the row.
    Nothing,
}

/// One place the focus can sit: a model by catalogue index, or a
/// not-installed row by provider name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stop {
    /// A model row.
    Model(usize),
    /// A not-installed provider row.
    Install(String),
}

/// The install state of every provider, by name, with the row focus.
/// Providers sit in a `Vec`, in the order their installs started.
#[derive(Default)]
pub(crate) struct Installs {
    /// Each provider that installs or failed to.
    entries: Vec<(String, Progress)>,
    /// The outcome lines of this open.
    outcomes: Vec<Outcome>,
    /// The job the loop has not picked up yet.
    pending: Option<InstallStart>,
    /// The not-installed row the person moved onto, by name.
    pub(super) focus: Option<String>,
}

impl Installs {
    /// Starts a new open: a running install carries on, since it is a
    /// fact about the world; the failures, the outcomes and the focus go.
    pub(crate) fn reset(&mut self) {
        self.entries
            .retain(|(_, progress)| *progress == Progress::Installing);
        self.outcomes.clear();
        self.focus = None;
    }

    /// Whether an install runs.
    pub(crate) fn busy(&self) -> bool {
        self.entries
            .iter()
            .any(|(_, progress)| *progress == Progress::Installing)
    }

    /// Where `provider`'s install stands, if it has one.
    pub(crate) fn progress(&self, provider: &str) -> Option<&Progress> {
        self.entries
            .iter()
            .find(|(name, _)| name == provider)
            .map(|(_, progress)| progress)
    }

    /// The outcome lines this open draws.
    pub(crate) fn outcomes(&self) -> &[Outcome] {
        &self.outcomes
    }

    /// Marks `start`'s provider installing and queues the job. One install
    /// runs at a time, so `false` with no change while another does: the
    /// install holds the extensions lock for its whole run.
    pub(crate) fn start(&mut self, start: InstallStart) -> bool {
        if self.busy() {
            return false;
        }
        self.entries.retain(|(name, _)| *name != start.provider);
        self.entries
            .push((start.provider.clone(), Progress::Installing));
        self.pending = Some(start);
        true
    }

    /// The job a start queued, once.
    pub(crate) fn take_start(&mut self) -> Option<InstallStart> {
        self.pending.take()
    }

    /// Shows `provider` as failed with `message`, without a job.
    pub(crate) fn fail(&mut self, provider: &str, message: String) {
        self.entries.retain(|(name, _)| name != provider);
        self.entries
            .push((provider.to_owned(), Progress::Failed(first_line(&message))));
    }

    /// Folds an install's result. A provider not installing is ignored, so
    /// a duplicate result makes no second outcome. A failure ends the run
    /// whether or not `visible`; a success stores its outcome only when
    /// the picker is `visible`, else the credential lines come back as
    /// notices.
    pub(crate) fn land(
        &mut self,
        provider: String,
        result: Result<ProviderInstall, ConfigureError>,
        visible: bool,
    ) -> Landed {
        let Some(at) = self
            .entries
            .iter()
            .position(|(name, progress)| *name == provider && *progress == Progress::Installing)
        else {
            return Landed::Nothing;
        };
        let clean = controls_to_space(&provider);
        match result {
            Ok(done) => {
                self.entries.remove(at);
                let login: Vec<LoginNeed> = done
                    .login
                    .into_iter()
                    .map(|need| LoginNeed {
                        provider: controls_to_space(&need.provider),
                        error: need.error.map(|error| controls_to_space(&error)),
                    })
                    .collect();
                if visible {
                    self.outcomes.push(Outcome {
                        provider: clean,
                        summary: done
                            .summary
                            .iter()
                            .map(|line| controls_to_space(line))
                            .collect(),
                        login,
                    });
                    Landed::Installed {
                        notices: Vec::new(),
                    }
                } else {
                    Landed::Installed {
                        notices: login.iter().map(credential_notice).collect(),
                    }
                }
            }
            Err(error) => {
                let message = first_line(&error.message);
                if visible {
                    if let Some(entry) = self.entries.get_mut(at) {
                        entry.1 = Progress::Failed(message);
                    }
                    Landed::Nothing
                } else {
                    self.entries.remove(at);
                    Landed::Notice(format!("Installing {clean} failed: {message}"))
                }
            }
        }
    }
}

/// The first line of `text` with control characters turned to spaces.
fn first_line(text: &str) -> String {
    controls_to_space(text.lines().next().unwrap_or(""))
}

/// What a provider's missing or unreadable key reads as, in a notice and
/// under the picker's list.
pub(crate) fn credential_text(need: &LoginNeed) -> String {
    match &need.error {
        None => format!(
            "{0} needs a credential: run `fiber login {0}`",
            need.provider
        ),
        Some(error) => format!("{}'s credential could not be read: {error}", need.provider),
    }
}

/// The notice for a provider needing a key: its code and text.
fn credential_notice(need: &LoginNeed) -> (&'static str, String) {
    let code = if need.error.is_none() {
        "credential_missing"
    } else {
        "credential_failed"
    };
    (code, credential_text(need))
}

impl ModelPicker {
    /// Whether the open picker draws not-installed rows: only the
    /// choosing picker does.
    pub(crate) fn install_visible(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|open| open.mode == Mode::Choose)
    }

    /// The not-installed providers the picker lists, in catalogue order,
    /// the query keeping those it matches by name.
    pub(crate) fn shown_installs(&self) -> Vec<String> {
        let Some(open) = self.open.as_ref() else {
            return Vec::new();
        };
        if open.mode != Mode::Choose {
            return Vec::new();
        }
        self.catalogue
            .installable
            .iter()
            .filter(|name| filter::matches_name(name, &open.query))
            .filter(|name| {
                let clean = controls_to_space(name);
                !self
                    .installs
                    .outcomes()
                    .iter()
                    .any(|outcome| outcome.provider == clean)
            })
            .cloned()
            .collect()
    }

    /// Where the focus sits: the row the person moved onto while it is
    /// listed, else the selected model while one shows, else nothing.
    /// Never another row.
    pub(crate) fn focus(&self) -> Option<Stop> {
        let open = self.open.as_ref()?;
        if let Some(name) = &self.installs.focus
            && self.shown_installs().contains(name)
        {
            return Some(Stop::Install(name.clone()));
        }
        let rows = self.shown_models();
        if rows.contains(&open.selected) {
            Some(Stop::Model(open.selected))
        } else {
            rows.first().copied().map(Stop::Model)
        }
    }

    /// The provider to install on Enter: the focused row's name. Only the
    /// choosing picker lists rows, so only it has one.
    pub(crate) fn install_target(&self) -> Option<String> {
        match self.focus()? {
            Stop::Install(name) => Some(name),
            Stop::Model(_) => None,
        }
    }

    /// Clicks `cell` of `row`: on a not-installed row the click focuses it,
    /// and the name cell answers the provider to install. Any other row
    /// answers `None` and changes nothing.
    pub(crate) fn install_click(&mut self, row: usize, cell: usize) -> Option<String> {
        let layout = self.layout()?;
        let Some(RowAt::Install(name)) = layout.get(row) else {
            return None;
        };
        self.installs.focus = Some(name.clone());
        (cell == 0).then(|| name.clone())
    }

    /// Asks `Stale`, keeping a wider `Every`.
    pub(crate) fn ask_stale(&mut self) {
        self.want = Some(match self.want {
            Some(Refresh::Every) => Refresh::Every,
            _ => Refresh::Stale,
        });
    }

    /// Folds an install's result. A success drops the provider from the
    /// list at once, comparing the cleaned name, and asks the one refresh
    /// that lists its models.
    pub(crate) fn land_install(
        &mut self,
        provider: String,
        result: Result<ProviderInstall, ConfigureError>,
    ) -> Landed {
        let visible = self.install_visible();
        let succeeded = result.is_ok();
        let clean = controls_to_space(&provider);
        let landed = self.installs.land(provider, result, visible);
        if succeeded && landed != Landed::Nothing {
            self.catalogue
                .installable
                .retain(|name| controls_to_space(name) != clean);
            self.prune_focus();
            self.ask_stale();
        }
        landed
    }

    /// Drops a row focus whose provider is no longer listed.
    pub(super) fn prune_focus(&mut self) {
        if let Some(name) = &self.installs.focus
            && !self.catalogue.installable.contains(name)
        {
            self.installs.focus = None;
        }
    }

    /// The places the focus walks, in order: the shown models, then the
    /// not-installed rows.
    pub(super) fn stops(&self) -> Vec<Stop> {
        let mut stops: Vec<Stop> = self.shown_models().into_iter().map(Stop::Model).collect();
        stops.extend(self.shown_installs().into_iter().map(Stop::Install));
        stops
    }

    /// Moves the focus onto `stop`.
    pub(super) fn go(&mut self, stop: Stop) {
        match stop {
            Stop::Model(index) => {
                if let Some(open) = self.open.as_mut() {
                    open.selected = index;
                }
                self.installs.focus = None;
            }
            Stop::Install(name) => self.installs.focus = Some(name),
        }
    }

    /// The shown models by catalogue index; none while closed.
    fn shown_models(&self) -> Vec<usize> {
        let Some(open) = self.open.as_ref() else {
            return Vec::new();
        };
        shown_in(
            &self.catalogue.models,
            &self.scoped,
            open.show_all,
            open.mode,
            open.target.as_ref(),
            &open.query,
        )
    }
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
