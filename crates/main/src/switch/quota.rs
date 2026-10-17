//! Non-waiting client quota, sharing providers and credential reads with
//! model switches (`docs/tools.md`, "Fetching").

use super::*;
use contract::ErrorCode;
use contract::events::{QuotaCredit, QuotaInfo, QuotaWindow};
use contract::shapes::{Failure, True};
use extensions::{CredentialPair, Quota, QuotaEntry, WindowUse};

pub(crate) type QuotaRead = Arc<dyn Fn() -> (Vec<QuotaInfo>, f64) + Send + Sync>;

/// The model-facing quota reader: one snapshot per call, answering from
/// the caches at once without waiting for loading, key reads or fetches
/// (`docs/tools.md`, "Fetching").
pub(crate) type ModelQuotaRead = Arc<dyn Fn() -> QuotaView + Send + Sync>;

/// One credential label the model sees: its entry, and whether its first
/// fetch is still in flight with no last value (`no value yet`). `pending`
/// is read before the entry, so a value landing between the two reads
/// shows the value, never `unreachable`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LabelQuota {
    /// The provider that owns the label.
    pub provider: String,
    /// The credential label.
    pub credential: String,
    /// What the model sees for the label.
    pub entry: QuotaEntry,
    /// The first fetch is in flight and nothing was ever fetched.
    pub pending: bool,
}

/// The model-facing quota snapshot: loading until the loader has checked
/// every provider, then one row per credential label.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum QuotaView {
    /// The loader has not checked every provider yet.
    Loading,
    /// One row per credential label, sorted by provider then credential.
    Loaded(Vec<LabelQuota>),
}

#[derive(Clone)]
struct Label {
    provider: String,
    label: String,
    lua: Arc<LuaProvider>,
    pair: CredentialPair,
    key: Option<Secret>,
}

#[derive(Default)]
struct Held {
    started: bool,
    /// The loader has checked every provider for `quota()`: a reader seeing
    /// false answers `loading`. Set once after the loop, never on shutdown.
    checked: bool,
    labels: Vec<Label>,
    failed: Vec<QuotaInfo>,
    /// Labels of providers that register no `quota()`: listed without
    /// reading their keys.
    silent: Vec<Silent>,
}

/// A label of a provider that registers no `quota()`: the model sees it as
/// `no quota reported`.
#[derive(Clone)]
struct Silent {
    provider: String,
    credential: String,
}

struct Store {
    providers: Vec<(String, ProviderData)>,
    config: Config,
    loader: Loader,
    started: Weak<StartLock>,
    reads: Arc<Reads>,
    notice_at: f64,
    state: Mutex<Held>,
    #[cfg(test)]
    labels_pause: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    /// Holds the loader after it starts a provider and before it checks
    /// whether the provider registers `quota()`: the loading test's hold.
    #[cfg(test)]
    register_pause: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    /// Holds the model view between its `quota_pending` read and its
    /// `quota_now` read: the read-order test's hold, so a fetch landing
    /// there is deterministic instead of a microsecond race.
    #[cfg(test)]
    between_pause: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    #[cfg(test)]
    loaded_note: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    /// Forces one provider's loader pass to fail its read: the
    /// non-closing `reads.run` error test's seam. Never set outside tests.
    #[cfg(test)]
    fail_reads: Mutex<Option<String>>,
}

impl Switching {
    /// Both quota readers over one store: the doors command's rows and the
    /// model-facing snapshot share one loader, so one session loads once.
    pub(crate) fn quota_readers(&self) -> (QuotaRead, ModelQuotaRead) {
        let store = self.quota_store();
        let doors = Arc::clone(&store);
        let model = store;
        (
            Arc::new(move || doors.answer()),
            Arc::new(move || model.entries()),
        )
    }

    #[allow(
        clippy::expect_used,
        reason = "Config validates quota.notice_at as a number and supplies its built-in default"
    )]
    fn quota_store(&self) -> Arc<Store> {
        let providers = self
            .loader
            .owners
            .iter()
            .filter_map(|(name, extension)| {
                self.registry.get(name).map(|data| {
                    let mut data = data.clone();
                    data.models.clear();
                    (extension.clone(), data)
                })
            })
            .collect();
        Arc::new(Store {
            providers,
            config: self.config.clone(),
            loader: self.loader.clone(),
            started: Arc::downgrade(&self.started),
            reads: self.reads.clone(),
            notice_at: self
                .config
                .get("quota.notice_at", None)
                .and_then(|(value, _)| value.as_f64())
                .expect("quota.notice_at has a numeric configured default"),
            state: Mutex::default(),
            #[cfg(test)]
            labels_pause: Mutex::default(),
            #[cfg(test)]
            register_pause: Mutex::default(),
            #[cfg(test)]
            between_pause: Mutex::default(),
            #[cfg(test)]
            loaded_note: Mutex::default(),
            #[cfg(test)]
            fail_reads: Mutex::default(),
        })
    }
}

impl Store {
    fn answer(self: &Arc<Self>) -> (Vec<QuotaInfo>, f64) {
        self.answer_with_spawn(|store| {
            std::thread::Builder::new()
                .name("fiber-quota-load".into())
                .spawn(move || store.load())
                .map(|_| ())
        })
    }

    /// The model-facing snapshot: every held, silent and failed label from
    /// cache, without waiting. Loading until the loader has checked every
    /// provider; afterwards one row per credential label. The pending flag
    /// is read before the entry, so a value landing between the two reads
    /// shows the value. Neither lock is held across a `quota_now` call.
    fn entries(self: &Arc<Self>) -> QuotaView {
        let (checked, labels, failed, silent) = self.snapshot_starting(|store| {
            std::thread::Builder::new()
                .name("fiber-quota-load".into())
                .spawn(move || store.load())
                .map(|_| ())
        });
        if !checked {
            return QuotaView::Loading;
        }
        let mut rows: Vec<LabelQuota> = Vec::new();
        for label in &labels {
            let pending = label.lua.quota_pending(&label.pair);
            #[cfg(test)]
            self.wait_between_reads();
            let entry = label.lua.quota_now(&label.pair, label.key.as_ref());
            rows.push(LabelQuota {
                provider: label.provider.clone(),
                credential: label.label.clone(),
                entry,
                pending,
            });
        }
        for silent in &silent {
            rows.push(LabelQuota {
                provider: silent.provider.clone(),
                credential: silent.credential.clone(),
                entry: QuotaEntry::NoQuota,
                pending: false,
            });
        }
        for failed in &failed {
            rows.push(LabelQuota {
                provider: failed.provider.clone(),
                credential: failed.credential.clone(),
                entry: QuotaEntry::Unreachable { last: None },
                pending: false,
            });
        }
        QuotaView::Loaded(rows)
    }

    /// Snapshots the cached rows, starting the loader once when it has not
    /// run: the one start-and-retry behind both readers. `checked` is copied
    /// in the same hold that claims the start, before any spawn, so a first
    /// call always sees loading. A failed spawn resets the claim, so the
    /// next call retries.
    fn snapshot_starting(
        self: &Arc<Self>,
        spawn: impl FnOnce(Arc<Self>) -> std::io::Result<()>,
    ) -> (bool, Vec<Label>, Vec<QuotaInfo>, Vec<Silent>) {
        let (start, checked, labels, failed, silent) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let start = !state.started;
            state.started = true;
            (
                start,
                state.checked,
                state.labels.clone(),
                state.failed.clone(),
                state.silent.clone(),
            )
        };
        if start && spawn(Arc::clone(self)).is_err() {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .started = false;
        }
        (checked, labels, failed, silent)
    }

    fn answer_with_spawn(
        self: &Arc<Self>,
        spawn: impl FnOnce(Arc<Self>) -> std::io::Result<()>,
    ) -> (Vec<QuotaInfo>, f64) {
        let (_, labels, mut quota, _) = self.snapshot_starting(spawn);
        quota.extend(labels.into_iter().filter_map(|label| {
            info(
                &label.provider,
                &label.label,
                label.lua.quota_now(&label.pair, label.key.as_ref()),
            )
        }));
        quota.sort_by(|a, b| (&a.provider, &a.credential).cmp(&(&b.provider, &b.credential)));
        (quota, self.notice_at)
    }

    fn load(self: &Arc<Self>) {
        #[cfg(test)]
        let _ended = Loaded(self);
        for (extension, data) in &self.providers {
            let labels = self.config.credentials().labels(data);
            if labels.is_empty() {
                continue;
            }
            let names = labels.clone();
            let provider = data.name.clone();
            let this = Arc::clone(self);
            let extension = extension.clone();
            let data = data.clone();
            // The non-closing `reads.run` error test's flag: read before
            // the read closure moves the provider's name away.
            #[cfg(test)]
            let fail = self.fail_reads.lock().unwrap().as_deref() == Some(data.name.as_str());
            let read = move || {
                let Some(started) = this.started.upgrade() else {
                    return (Vec::new(), Vec::new(), Vec::new());
                };
                let failures = labels
                    .iter()
                    .map(|label| failed(&data.name, label))
                    .collect();
                let lua = started.start(&this.loader, &this.config, &data.name, &extension);
                drop(started);
                let lua = match lua {
                    Ok(lua) => lua,
                    Err(_) => return (Vec::new(), failures, Vec::new()),
                };
                #[cfg(test)]
                this.pause_before_register();
                match lua.registers("quota") {
                    Ok(false) => {
                        return (Vec::new(), Vec::new(), silent(&data.name, &labels));
                    }
                    Err(_) => return (Vec::new(), failures, Vec::new()),
                    Ok(true) => {}
                }
                #[cfg(test)]
                this.pause_before_labels();
                let mut held = Vec::new();
                let mut failures = Vec::new();
                for label in labels {
                    let run = |command: &mut std::process::Command, timeout| {
                        this.reads.credential(command, timeout, &*this.loader.clock)
                    };
                    let key =
                        crate::lua_providers::session_credential(Some(&lua), &data, &label, || {
                            crate::credential::switch_credential(&this.config, &data, &label, &run)
                                .map(|read| read.secret)
                        });
                    match key {
                        Ok((key, _)) => held.push(Label {
                            provider: data.name.clone(),
                            pair: CredentialPair::for_provider(&data, &label),
                            label,
                            lua: lua.clone(),
                            key,
                        }),
                        Err(_) => failures.push(failed(&data.name, &label)),
                    }
                }
                (held, failures, Vec::new())
            };
            // A forced failure stands in for the `reads.run` spawn error
            // no test can produce on purpose: the loader still records
            // that provider's labels as failed and checks the rest.
            #[cfg(test)]
            let outcome = if fail {
                Err(doors::failure(ErrorCode::IoFailed, "the read failed"))
            } else {
                self.reads.run(read)
            };
            #[cfg(not(test))]
            let outcome = self.reads.run(read);
            let (keep_going, held) = {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                apply_outcome(&mut state, &provider, &names, outcome)
            };
            if !keep_going {
                return;
            }
            for label in held {
                label.lua.refresh_quota(&label.pair, label.key.as_ref());
            }
        }
        // Every provider checked without a shutdown: later readers list
        // labels instead of loading. A dropped session leaves it false.
        if self.started.upgrade().is_some() {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .checked = true;
        }
    }

    #[cfg(test)]
    fn pause_before_register(&self) {
        Self::wait_on(&self.register_pause);
    }

    /// Fails one provider's next loader pass with a non-closing read
    /// error: the loader records that provider's labels as failed and
    /// checks the rest.
    #[cfg(test)]
    fn fail_reads_for(&self, provider: &str) {
        *self.fail_reads.lock().unwrap() = Some(provider.to_owned());
    }

    #[cfg(test)]
    fn pause_at_register(
        &self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        Self::hold_on(&self.register_pause, entered, release);
    }

    #[cfg(test)]
    fn pause_between_reads(
        &self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        Self::hold_on(&self.between_pause, entered, release);
    }

    #[cfg(test)]
    fn wait_between_reads(&self) {
        Self::wait_on(&self.between_pause);
    }

    /// One test hold over a pause channel: the loader signals it reached
    /// the hold and blocks until the test releases it.
    #[cfg(test)]
    fn wait_on(
        pause: &Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    ) {
        let pause = pause.lock().unwrap().take();
        if let Some((entered, release)) = pause {
            let _sent = entered.send(());
            let _released = release.recv();
        }
    }

    /// Arms one test hold over a pause channel.
    #[cfg(test)]
    fn hold_on(
        pause: &Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        *pause.lock().unwrap() = Some((entered, release));
    }

    #[cfg(test)]
    fn pause_at_labels(
        &self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        *self.labels_pause.lock().unwrap() = Some((entered, release));
    }

    #[cfg(test)]
    fn pause_before_labels(&self) {
        let pause = self.labels_pause.lock().unwrap().take();
        if let Some((entered, release)) = pause {
            let _sent = entered.send(());
            let _released = release.recv();
        }
    }

    #[cfg(test)]
    fn note_loaded(&self, sender: std::sync::mpsc::Sender<()>) {
        *self.loaded_note.lock().unwrap() = Some(sender);
    }

    #[cfg(test)]
    fn held_instance(&self, name: &str) -> Arc<LuaProvider> {
        self.state
            .lock()
            .unwrap()
            .labels
            .iter()
            .find(|label| label.provider == name)
            .unwrap()
            .lua
            .clone()
    }
}

#[cfg(test)]
struct Loaded<'a>(&'a Store);
#[cfg(test)]
impl Drop for Loaded<'_> {
    fn drop(&mut self) {
        let sender = self.0.loaded_note.lock().unwrap().clone();
        if let Some(sender) = sender {
            let _sent = sender.send(());
        }
    }
}

fn failed(provider: &str, label: &str) -> QuotaInfo {
    QuotaInfo {
        provider: provider.into(),
        credential: label.into(),
        windows: None,
        credit: None,
        unreachable: Some(True),
    }
}

/// One provider's loaded rows: its held labels, its failed rows and its
/// silent labels.
type LoadRows = (Vec<Label>, Vec<QuotaInfo>, Vec<Silent>);

/// One provider's loader outcome folded into the held rows, under one hold:
/// a loaded provider's labels, failures and silent labels all at once. A
/// shutdown (`Closing`) stops the loader with later providers unchecked;
/// any other failed read records that provider's labels as failed and
/// continues, while the other labels still answer (`docs/tools.md`,
/// "Fetching"). Returns whether to keep loading, with the just-held
/// labels to fetch.
fn apply_outcome(
    state: &mut Held,
    provider: &str,
    labels: &[String],
    outcome: Result<LoadRows, Failure>,
) -> (bool, Vec<Label>) {
    match outcome {
        Ok((held, failed, silent)) => {
            state.labels.extend(held.clone());
            state.failed.extend(failed);
            state.silent.extend(silent);
            (true, held)
        }
        Err(failure) if failure.code == ErrorCode::Closing => (false, Vec::new()),
        Err(_) => {
            state
                .failed
                .extend(labels.iter().map(|label| failed(provider, label)));
            (true, Vec::new())
        }
    }
}

/// Labels of a provider that registers no `quota()`: named without reading
/// any key.
fn silent(provider: &str, labels: &[String]) -> Vec<Silent> {
    labels
        .iter()
        .map(|label| Silent {
            provider: provider.to_owned(),
            credential: label.clone(),
        })
        .collect()
}

fn info(provider: &str, label: &str, entry: QuotaEntry) -> Option<QuotaInfo> {
    let (value, unreachable) = match entry {
        QuotaEntry::Reported(value) => (value, None),
        QuotaEntry::Unreachable {
            last: Some((value, _)),
        } => (value, Some(True)),
        QuotaEntry::NoQuota | QuotaEntry::Unreachable { last: None } => return None,
    };
    let mut info = QuotaInfo {
        provider: provider.into(),
        credential: label.into(),
        windows: None,
        credit: None,
        unreachable,
    };
    match value {
        Quota::Windows(windows) => {
            if windows.is_empty() {
                return None;
            }
            info.windows = Some(
                windows
                    .into_iter()
                    .map(|window| {
                        let (percent_used, reset) = match window.used {
                            WindowUse::Percent { used, .. } => (Some(used), None),
                            WindowUse::Reset => (None, Some(True)),
                        };
                        QuotaWindow {
                            name: window.name,
                            percent_used,
                            reset,
                        }
                    })
                    .collect(),
            );
        }
        Quota::Credit { remaining, limit } => info.credit = Some(QuotaCredit { remaining, limit }),
    }
    Some(info)
}

#[cfg(test)]
#[path = "quota_tests.rs"]
mod tests;

#[cfg(test)]
mod spawn_tests {
    use crate::switch::tests::{fixture, switching};

    #[test]
    fn a_failed_quota_loader_spawn_retries_but_a_successful_one_does_not() {
        let fixture = fixture("fiber-quota-spawn-retry");
        let switching = switching(&fixture, &[]);
        let store = switching.quota_store();
        let failed = store.answer_with_spawn(|_| Err(std::io::Error::other("spawn refused")));
        assert!(failed.0.is_empty());
        assert_eq!(failed.1, 80.0);
        assert!(!store.state.lock().unwrap().started);

        let mut starts = 0;
        store.answer_with_spawn(|store| {
            starts += 1;
            store.load();
            Ok(())
        });
        assert_eq!(starts, 1, "the next command retries the failed spawn");
        assert!(store.state.lock().unwrap().started);
        store.answer_with_spawn(|_| panic!("a successful loader is started only once"));
    }
}
