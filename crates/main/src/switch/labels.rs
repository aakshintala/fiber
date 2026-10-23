//! Credential queries and the provider a credential command would switch now.

use super::Switching;
use contract::events::CredentialLabels;
use std::sync::{Arc, Mutex};

pub(crate) struct CredentialReaders {
    pub(crate) prepare: r#loop::Prepare,
    pub(crate) labels: Arc<dyn Fn() -> Option<CredentialLabels> + Send + Sync>,
    pub(crate) admitted: Arc<dyn Fn() -> String + Send + Sync>,
}

impl Switching {
    /// Tracks the last admitted model, which is the queued switch's model
    /// until apply, and the session model afterward, including a no-op switch.
    pub(crate) fn credential_readers(
        self: &Arc<Self>,
        prepare: r#loop::Prepare,
        started: String,
    ) -> CredentialReaders {
        let model = Arc::new(Mutex::new(started));
        let switched = Arc::clone(&model);
        let prepare: r#loop::Prepare = Arc::new(move |args, label, chosen| {
            let prepared = prepare(args, label, chosen)?;
            *support::lock(&switched) = prepared.model.reference.clone();
            Ok(prepared)
        });
        let admitted: Arc<dyn Fn() -> String + Send + Sync> = Arc::new(move || {
            let model = support::lock(&model);
            model.split('/').next().unwrap_or_default().to_owned()
        });
        let provider = Arc::clone(&admitted);
        let switching = Arc::downgrade(self);
        let labels = Arc::new(move || {
            let switching = switching.upgrade()?;
            let provider = provider();
            let data = switching
                .registry
                .get(&provider)
                .filter(|data| !crate::scripted::is_scripted(data));
            let (credentials, credential) = data.map_or_else(
                || (Vec::new(), None),
                |data| {
                    let credentials = switching.startup_config.credentials().labels(data);
                    let credential = support::lock(&switching.current).get(&provider).cloned();
                    (credentials, credential)
                },
            );
            Some(CredentialLabels {
                provider,
                credentials,
                credential,
            })
        });
        CredentialReaders {
            prepare,
            labels,
            admitted,
        }
    }
}

#[cfg(test)]
#[path = "labels_tests.rs"]
mod tests;
