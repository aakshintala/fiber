//! Non-waiting client quota, sharing providers and credential reads with
//! model switches (`docs/tools.md`, "Fetching").

use super::*;
use contract::events::{QuotaCredit, QuotaInfo, QuotaWindow};
use contract::shapes::True;
use extensions::{CredentialPair, Quota, QuotaEntry, WindowUse};

pub(crate) type QuotaRead = Arc<dyn Fn() -> (Vec<QuotaInfo>, f64) + Send + Sync>;

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
    labels: Vec<Label>,
    failed: Vec<QuotaInfo>,
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
    #[cfg(test)]
    loaded_note: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl Switching {
    pub(crate) fn quota_reader(&self) -> QuotaRead {
        let store = self.quota_store();
        Arc::new(move || store.answer())
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
            loaded_note: Mutex::default(),
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

    fn answer_with_spawn(
        self: &Arc<Self>,
        spawn: impl FnOnce(Arc<Self>) -> std::io::Result<()>,
    ) -> (Vec<QuotaInfo>, f64) {
        let (start, labels, mut quota) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let start = !state.started;
            state.started = true;
            (start, state.labels.clone(), state.failed.clone())
        };
        if start && spawn(Arc::clone(self)).is_err() {
            self.state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .started = false;
        }
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
            let this = Arc::clone(self);
            let extension = extension.clone();
            let data = data.clone();
            let outcome = self.reads.run(move || {
                let Some(started) = this.started.upgrade() else {
                    return (Vec::new(), Vec::new());
                };
                let failures = labels
                    .iter()
                    .map(|label| failed(&data.name, label))
                    .collect();
                let lua = started.start(&this.loader, &this.config, &data.name, &extension);
                drop(started);
                let lua = match lua {
                    Ok(lua) => lua,
                    Err(_) => return (Vec::new(), failures),
                };
                match lua.registers("quota") {
                    Ok(false) => return (Vec::new(), Vec::new()),
                    Err(_) => return (Vec::new(), failures),
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
                (held, failures)
            });
            let (held, failed) = match outcome {
                Ok(outcome) => outcome,
                Err(_) => return,
            };
            {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.labels.extend(held.clone());
                state.failed.extend(failed);
            }
            for label in held {
                label.lua.refresh_quota(&label.pair, label.key.as_ref());
            }
        }
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
