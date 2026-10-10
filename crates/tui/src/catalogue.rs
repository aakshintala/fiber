//! The installed models, as the picker lists them (`docs/tui.md`,
//! "Swapped views"): one entry per model of every installed provider,
//! read on a thread off the loop. The loop asks for at most one read at
//! a time; a read asked while one runs waits, and the widest waiting win
//! (`docs/model-routing.md`, "Model discovery").

use std::sync::Arc;
use std::sync::mpsc::Sender;

/// One model of one installed provider, as the picker lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    /// The model as a session names it, `provider/model`.
    pub reference: String,
    /// Its provider's name.
    pub provider: String,
    /// The model's id, as the vendor spells it.
    pub id: String,
    /// The model's display name, when its provider data names one.
    pub name: Option<String>,
    /// The thinking levels the model takes, as words.
    pub levels: Vec<String>,
    /// The model's own default level, when it names one.
    pub default_level: Option<String>,
    /// The level configuration names for this model, when it names one.
    pub configured: Option<String>,
    /// The role names marking this model.
    pub roles: Vec<String>,
}

/// The installed models, and the notices their read gave.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalogue {
    /// One entry per model of every installed provider, providers sorted
    /// by name, models in list order.
    pub models: Vec<ModelEntry>,
    /// One notice per read, in read order.
    pub notices: Vec<String>,
}

/// How thorough a model-list read is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Refresh {
    /// The cached lists, starting no extension.
    Cached,
    /// The cached lists, refreshing the stale ones with the age check.
    Stale,
    /// The cached lists, refreshing every list whatever the cache holds.
    Every,
}

/// Reads the model lists at `refresh`: the cached copy at once, and,
/// for [`Refresh::Stale`] and [`Refresh::Every`], the background refresh
/// before answering.
pub type ReadModels = Arc<dyn Fn(Refresh) -> Result<Catalogue, String> + Send + Sync>;

/// The loop's one model-list read at a time.
pub(crate) struct Reader {
    /// How the lists are read; `None` in the jigs, which ask nothing.
    read: Option<ReadModels>,
    /// Whether a read thread runs.
    running: bool,
    /// The widest refresh asked while one runs.
    queued: Option<Refresh>,
}

impl Reader {
    /// A reader that reads with `read`, or asks nothing without one.
    pub(crate) fn new(read: Option<ReadModels>) -> Self {
        Self {
            read,
            running: false,
            queued: None,
        }
    }

    /// Asks for a read at `refresh`: it starts at once, unless one runs,
    /// when the widest asked waits. Without a read, asking does nothing.
    pub(crate) fn ask(&mut self, refresh: Refresh, out: &Sender<crate::Input>) {
        let Some(read) = &self.read else {
            return;
        };
        if self.running {
            // The queue keeps the widest asked: a wider refresh answers
            // for a narrower one.
            self.queued = Some(match self.queued {
                Some(queued) => queued.max(refresh),
                None => refresh,
            });
            return;
        }
        self.running = true;
        let read = Arc::clone(read);
        let out = out.clone();
        // A read whose send fails finds the loop closed, and ends: the
        // answer is dropped with the send's error.
        drop(crate::sources::builder("tui-models").spawn(move || {
            drop(out.send(crate::Input::Models(read(refresh))));
        }));
    }

    /// A read answered: the widest waiting starts, if one waits.
    pub(crate) fn done(&mut self, out: &Sender<crate::Input>) {
        self.running = false;
        if let Some(refresh) = self.queued.take() {
            self.ask(refresh, out);
        }
    }
}

#[cfg(test)]
#[path = "catalogue_tests.rs"]
mod tests;
