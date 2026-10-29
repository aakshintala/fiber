//! The installed models, as the picker lists them (`docs/tui.md`,
//! "Swapped views"): one entry per model of every installed provider,
//! read on a thread off the loop. The loop asks for at most one read at
//! a time; a read asked while one runs waits, and the widest waiting win
//! (`docs/model-routing.md`, "Model discovery").

use std::io;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::thread;

use contract::events::Notice;

/// A model's prices in integer micro-dollars per million tokens, so
/// `ModelEntry` keeps `Eq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Price {
    /// The rebuild rate: `cache_write`, or `input` when the model names none.
    pub micros_per_mtok: u64,
    /// The input price.
    pub input: u64,
    /// The output price.
    pub output: u64,
    /// Higher input sizes, ascending by `input_tokens_above`.
    pub tiers: Vec<PriceTier>,
}

/// One size tier's prices, in micro-dollars per million tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriceTier {
    /// Input tokens above which this tier's prices apply.
    pub input_tokens_above: u64,
    /// The rebuild rate: `cache_write` when above zero, else `input`.
    pub micros_per_mtok: u64,
    /// The input price, in micro-dollars per million tokens.
    pub input: u64,
    /// The output price, in micro-dollars per million tokens.
    pub output: u64,
}

impl Price {
    /// The last tier whose threshold the whole input exceeds.
    pub(crate) fn tier_at(&self, tokens: u64) -> Option<&PriceTier> {
        self.tiers
            .iter()
            .rev()
            .find(|tier| tier.input_tokens_above < tokens)
    }
}

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
    /// The model's prices, when its provider data names a cost.
    pub price: Option<Price>,
}

/// One provider's list time: when its cached copy was written and
/// whether it is stale (`docs/model-routing.md`, "Model discovery").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListAge {
    /// The provider's name.
    pub provider: String,
    /// When its cached copy was written, in milliseconds since the
    /// epoch; `None` with no cached copy.
    pub updated_ms: Option<u64>,
    /// Whether the list is stale: older than `refresh_after`, or with
    /// no cached copy at all.
    pub stale: bool,
}

/// The installed models, and the notices their read gave.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalogue {
    /// One entry per model of every installed provider, providers sorted
    /// by name, models in list order.
    pub models: Vec<ModelEntry>,
    /// One notice per read, in read order: [`Notice`], code and message,
    /// so each surface shows it under its own code.
    pub notices: Vec<Notice>,
    /// One list time per installed provider.
    pub lists: Vec<ListAge>,
    /// Each first-party provider no installed extension declares, sorted by
    /// name (`docs/extensions.md`, "A fresh install").
    pub installable: Vec<String>,
    /// One group per provider and setting whose placeholder has no
    /// value, in provider then setting order: the models the terminal
    /// counts in the picker instead of noticing at launch
    /// (`docs/model-routing.md`, "A per-account host").
    pub left_out: Vec<LeftOutGroup>,
}

/// One model left out for a placeholder with no value: its id and its
/// display name, when its provider data names one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftOutModel {
    /// The model's id, as the vendor spells it.
    pub id: String,
    /// Its display name, when the provider data names one.
    pub name: Option<String>,
}

/// The models of one provider left out for one placeholder with no
/// value, as the picker counts them (`docs/tui.md`, "The model picker").
/// `file` is the declaring extension's global configuration file,
/// relative to Fiber home, as read: `main` builds it and the terminal
/// renders it verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftOutGroup {
    /// The provider's name.
    pub provider: String,
    /// The extension that registered the provider, as it is registered.
    pub extension: String,
    /// The declaring extension's global configuration file, relative to
    /// Fiber home.
    pub file: String,
    /// The placeholder with no value.
    pub setting: String,
    /// Whether `fiber login <provider>` asks for the setting.
    pub login: bool,
    /// The models left out, in model order.
    pub models: Vec<LeftOutModel>,
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
        self.ask_with(refresh, out, |builder, body| builder.spawn(body));
    }

    /// Asks as [`Reader::ask`] does, with the thread spawn as a closure
    /// so tests fail it. A failed spawn fails every owed read with the
    /// same `terminal:` message but leaves the reader busy until the loop
    /// consumes those errors: freeing now would let a later ask start
    /// over the unconsumed errors, owing three answers (the failure's
    /// error, the running read, the queued one) while the picker counts
    /// two. Each error's [`Reader::done`] frees the reader and starts
    /// what waited meanwhile; nothing stays queued.
    pub(crate) fn ask_with(
        &mut self,
        refresh: Refresh,
        out: &Sender<crate::Input>,
        spawn: impl FnOnce(
            thread::Builder,
            Box<dyn FnOnce() + Send + 'static>,
        ) -> io::Result<thread::JoinHandle<()>>,
    ) {
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
        let thread_out = out.clone();
        // A read whose send fails finds the loop closed, and ends: the
        // answer is dropped with the send's error.
        match spawn(
            crate::sources::builder("tui-models"),
            Box::new(move || {
                drop(thread_out.send(crate::Input::Models(read(refresh))));
            }),
        ) {
            Ok(_) => {}
            Err(source) => {
                // No thread runs, but the reader stays busy until the
                // loop consumes these errors: each one answers an owed
                // read, and freeing now would let a later ask start over
                // the unconsumed errors. The asked one fails, and the
                // queued one when one waits; nothing stays queued, and
                // each error's `done` frees the reader and starts what
                // waited meanwhile.
                let queued = self.queued.take();
                let message = format!("terminal: could not start the model-list reader: {source}");
                drop(out.send(crate::Input::Models(Err(message.clone()))));
                if queued.is_some() {
                    drop(out.send(crate::Input::Models(Err(message))));
                }
            }
        }
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
