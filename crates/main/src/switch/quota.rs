//! Non-waiting client quota, sharing providers and credential reads with
//! model switches (`docs/tools.md`, "Fetching").
//!
//! Timing bounds: the fetch path answers within 8 s of its fetches
//! starting, plus loading.
//!
//! | Wait | Bound |
//! |---|---|
//! | Loader wait | `LOADER_BUDGET` on the loader clock |
//! | One label's fetch | its flight's own deadline (start + 8 s) |
//! | Sequential label waits | still ~8 s after fetch start: every flight
//! started together shares one deadline |
//! | Loader and key reads | the existing read timeouts, unchanged |

use std::sync::Condvar;
use std::time::Duration;

use super::*;
use contract::ErrorCode;
use contract::events::{QuotaCredit, QuotaInfo, QuotaWindow};
use contract::shapes::{Failure, True};
use extensions::{CredentialPair, Quota, QuotaEntry, WindowUse};

/// How long the fetch path waits for a cold loader to check every
/// provider: comfortably above the key reads' own timeouts, which bound
/// the loader's work. Past it the fetch path answers `unreachable` for
/// every startup label instead of hanging a `/quota`.
const LOADER_BUDGET: Duration = Duration::from_secs(30);

/// How long one loader wait parks between predicate rechecks: a missed
/// exit is rechecked instead of slept through, and a fake-clock advance
/// past the budget is noticed at most this late.
const LOADER_POLL: Duration = Duration::from_millis(50);

/// The driver quota reader: without `fetch` it answers from the caches at
/// once; with `fetch` it starts one fetch per key-read label, whatever
/// the cache's age, and answers once every fetch it started has ended.
/// The cached path ignores the flag. One reader map per flag value is not
/// needed: the fetch path shares flights label-wise underneath.
pub(crate) type QuotaRead = Arc<dyn Fn(bool) -> (Vec<QuotaInfo>, f64) + Send + Sync>;

/// The notice reader: only what is already cached, never fetching. The
/// loop's notice path takes this shape, so it stays as it is.
pub(crate) type NoticeQuotaRead = Arc<dyn Fn() -> (Vec<QuotaInfo>, f64) + Send + Sync>;

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
    /// Credential labels and sources remain fixed across reloads.
    startup_config: Config,
    now: Arc<Mutex<super::Reloadable>>,
    started: Weak<StartLock>,
    reads: Arc<Reads>,
    state: Mutex<Held>,
    /// Signalled at every `load()` exit, success or shutdown: the fetch
    /// path waits on it while the loader has not checked every provider.
    loaded: Condvar,
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

pub(crate) struct QuotaReaders {
    pub(crate) door: QuotaRead,
    pub(crate) model: ModelQuotaRead,
    pub(crate) notice: NoticeQuotaRead,
}

impl Switching {
    /// Three quota readers over one store: commands and model snapshots
    /// share the loader; notices read only what is already cached.
    pub(crate) fn quota_readers(&self) -> QuotaReaders {
        let store = self.quota_store();
        let doors = Arc::clone(&store);
        let model = Arc::clone(&store);
        QuotaReaders {
            door: Arc::new(move |fetch| {
                if fetch {
                    doors.answer_fetch()
                } else {
                    doors.answer()
                }
            }),
            model: Arc::new(move || model.entries()),
            notice: Arc::new(move || store.cached()),
        }
    }

    fn quota_store(&self) -> Arc<Store> {
        let loader = self.reloadable().loader;
        let providers = loader
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
            startup_config: self.startup_config.clone(),
            now: self.now.clone(),
            started: Arc::downgrade(&self.started),
            reads: self.reads.clone(),
            state: Mutex::default(),
            loaded: Condvar::new(),
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
    /// Starts the loader once, named for the quota waits: the one spawn
    /// behind the cached, model and fetch readers.
    fn spawn_loader(store: Arc<Self>) -> std::io::Result<()> {
        std::thread::Builder::new()
            .name("fiber-quota-load".into())
            .spawn(move || store.load())
            .map(|_| ())
    }

    fn answer(self: &Arc<Self>) -> (Vec<QuotaInfo>, f64) {
        self.answer_with_spawn(Self::spawn_loader)
    }

    /// The model-facing snapshot: every held, silent and failed label from
    /// cache, without waiting. Loading until the loader has checked every
    /// provider; afterwards one row per credential label. The pending flag
    /// is read before the entry, so a value landing between the two reads
    /// shows the value. Neither lock is held across a `quota_now` call.
    fn entries(self: &Arc<Self>) -> QuotaView {
        let (checked, labels, failed, silent) = self.snapshot_starting(Self::spawn_loader);
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
            let mut state = support::lock(&self.state);
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
            support::lock(&self.state).started = false;
        }
        (checked, labels, failed, silent)
    }

    fn cached(self: &Arc<Self>) -> (Vec<QuotaInfo>, f64) {
        let (labels, mut quota) = {
            let state = support::lock(&self.state);
            (state.labels.clone(), state.failed.clone())
        };
        quota.extend(labels.into_iter().filter_map(|label| {
            info(
                &label.provider,
                &label.label,
                label.lua.quota_cached(&label.pair),
            )
        }));
        self.with_threshold(quota)
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
        self.with_threshold(quota)
    }

    // debt: 810 lines, extract loader/fetch logic into `quota/{fetch,loader}.rs` at a real
    // module boundary when those paths are in the writing fence.
    /// The fetch path: one fetch per label whose key read succeeded,
    /// whatever the cache's age, answering once every fetch it started
    /// has ended (`docs/invocation.md`, "What each command does",
    /// `quota`). A label whose fetch failed or timed out answers
    /// `unreachable`; with zero labels it answers empty at once. Neither
    /// lock is held across a fetch or a wait: labels are cloned under the
    /// lock like the cached path clones them.
    fn answer_fetch(self: &Arc<Self>) -> (Vec<QuotaInfo>, f64) {
        let (checked, labels, failed, _) = self.snapshot_starting(Self::spawn_loader);
        let (labels, mut quota) = match checked {
            true => (labels, failed),
            // A cold cache: wait for the loader's label list instead of
            // answering the partial snapshot as complete.
            false => match self.wait_loaded() {
                Some((labels, failed)) => (labels, failed),
                // The budget expired or the session shut down with the
                // loader still unchecked: no label resolved, so every
                // startup label answers `unreachable` through the failure
                // mapper, never partial and never empty as complete.
                None => return self.with_threshold(self.startup_unreachable()),
            },
        };
        // Every flight starts before any wait: the flights share one
        // deadline, so the sequential waits still end ~8 s after the
        // fetches start instead of staggering a fresh 8 s per label.
        let waits = labels
            .iter()
            .map(|label| {
                let wait = label.lua.force_start_quota(&label.pair, label.key.as_ref());
                (label, wait)
            })
            .collect::<Vec<_>>();
        quota.extend(waits.into_iter().filter_map(|(label, wait)| {
            fetch_row(
                &label.provider,
                &label.label,
                label.lua.wait_quota(&label.pair, wait),
            )
        }));
        self.with_threshold(quota)
    }

    /// Waits for the loader to check every provider, then resnapshots the
    /// label list. `None` when the `LOADER_BUDGET` expired or the session
    /// shut down while still unchecked. The predicate rechecks `checked`
    /// under the lock, so a notify that landed before the wait and a
    /// spurious wakeup both resnapshot harmlessly.
    fn wait_loaded(self: &Arc<Self>) -> Option<(Vec<Label>, Vec<QuotaInfo>)> {
        let clock = support::lock(&self.now).loader.clock.clone();
        let start = clock.now();
        let end = start.checked_add(LOADER_BUDGET).unwrap_or(start);
        let mut guard = support::lock(&self.state);
        loop {
            if guard.checked {
                return Some((guard.labels.clone(), guard.failed.clone()));
            }
            // A dropped session ends the wait: the loader already exited
            // unchecked, so its labels answer `unreachable`.
            self.started.upgrade()?;
            if clock.now() >= end {
                return None;
            }
            let parked = support::clock::park(
                clock.as_ref(),
                Some(end),
                Some(LOADER_POLL),
                &self.loaded,
                guard,
                |held| held.checked,
            );
            let Some(next) = parked else {
                // The clock never parks without invoking the wait; ending
                // the wait answers instead of spinning past it.
                return None;
            };
            guard = next;
        }
    }

    /// Every startup credential label as `unreachable`: the loader never
    /// resolved any of them, so none may answer a value and none may go
    /// missing. Providers with no labels contribute none, as in loading.
    fn startup_unreachable(&self) -> Vec<QuotaInfo> {
        let mut rows = Vec::new();
        for (_, data) in &self.providers {
            for label in self.startup_config.credentials().labels(data) {
                rows.push(failed(&data.name, &label));
            }
        }
        rows
    }

    #[allow(
        clippy::expect_used,
        reason = "Config validates quota.notice_at and supplies its numeric default"
    )]
    fn with_threshold(&self, mut quota: Vec<QuotaInfo>) -> (Vec<QuotaInfo>, f64) {
        quota.sort_by(|a, b| (&a.provider, &a.credential).cmp(&(&b.provider, &b.credential)));
        let config = support::lock(&self.now).config.clone();
        let notice_at = config
            .get("quota.notice_at", None)
            .and_then(|(value, _)| value.as_f64())
            .expect("quota.notice_at has a numeric configured default");
        (quota, notice_at)
    }

    fn load(self: &Arc<Self>) {
        // Notified at every exit, success or shutdown: the fetch path's
        // loader wait ends with the loader, never past it.
        let _loaded = NotifyLoaded(self);
        #[cfg(test)]
        let _ended = Loaded(self);
        let loader = support::lock(&self.now).loader.clone();
        for (extension, data) in &self.providers {
            let labels = self.startup_config.credentials().labels(data);
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
            let loader = loader.clone();
            let read = move || {
                let Some(started) = this.started.upgrade() else {
                    return (Vec::new(), Vec::new(), Vec::new());
                };
                let failures = labels
                    .iter()
                    .map(|label| failed(&data.name, label))
                    .collect();
                let lua = started.start(&loader, &this.startup_config, &data.name, &extension);
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
                        this.reads.credential(command, timeout, &*loader.clock)
                    };
                    let key =
                        crate::lua_providers::session_credential(Some(&lua), &data, &label, || {
                            crate::credential::switch_credential(
                                &this.startup_config,
                                &data,
                                &label,
                                &run,
                            )
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
                self.reads.run("credential read", read)
            };
            #[cfg(not(test))]
            let outcome = self.reads.run("credential read", read);
            let (keep_going, held) = {
                let mut state = support::lock(&self.state);
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
            support::lock(&self.state).checked = true;
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
/// Signals the loader's end at every `load()` exit, success or shutdown:
/// the fetch path's loader wait ends with the loader, never past it.
struct NotifyLoaded<'a>(&'a Store);
impl Drop for NotifyLoaded<'_> {
    // Exempted: without the notify the 50ms poll still ends the loader
    // wait, so no consumer-visible behaviour changes; pinning the prompt
    // wake would need a real-clock timing assertion, which docs/testing.md
    // ("Waits and timeouts") bans, and the wait's own budget bounds it.
    #[cfg_attr(false, mutants::skip)]
    fn drop(&mut self) {
        self.0.loaded.notify_all();
    }
}
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

/// One awaited fetch as its answer row: the value where the fetch
/// resolved one, `unreachable` with no value where the fetch failed or
/// timed out with nothing ever fetched. A label reporting no quota stays
/// absent, as in the cached path.
fn fetch_row(provider: &str, label: &str, entry: QuotaEntry) -> Option<QuotaInfo> {
    match entry {
        QuotaEntry::Unreachable { last: None } => Some(failed(provider, label)),
        QuotaEntry::Reported(_)
        | QuotaEntry::NoQuota
        | QuotaEntry::Unreachable { last: Some(_) } => info(provider, label, entry),
    }
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
                        let (percent_used, reset, resets_at) = match window.used {
                            WindowUse::Percent { used, resets_at } => {
                                (Some(used), None, resets_at.map(contract::clock::wall_ms))
                            }
                            WindowUse::Reset => (None, Some(True), None),
                        };
                        QuotaWindow {
                            name: window.name,
                            percent_used,
                            reset,
                            resets_at,
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
