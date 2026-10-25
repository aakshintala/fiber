//! The session's extensions, loaded once its configuration is read
//! (`docs/extensions.md`, "Registering"): `extensions_loaded` after
//! `fiber_started`, and the hooks on the loop when any registered one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The extension set installed in a running session.
pub(crate) struct Current(Mutex<Arc<SessionExtensions>>);

impl Current {
    pub(crate) fn new(extensions: Arc<SessionExtensions>) -> Self {
        Self(Mutex::new(extensions))
    }
    pub(crate) fn get(&self) -> Arc<SessionExtensions> {
        support::lock(&self.0).clone()
    }
    pub(crate) fn swap(&self, extensions: Arc<SessionExtensions>) {
        *support::lock(&self.0) = extensions;
    }
}

use contract::hook::Hooks;
use extensions::SessionExtensions;
use log::Log;
use r#loop::Loop;

/// Where the session's repository extensions are read: the workspace
/// whose declarations count (`None`: the launch directory) and the
/// content the session skipped.
#[derive(Debug, Clone, Default)]
pub(crate) struct RepositoryScope {
    pub(crate) workspace: Option<PathBuf>,
    pub(crate) skipped: Vec<r#loop::OfferKey>,
}

/// Loads the repository set the session starts or reloads from: the
/// scope's workspace, or the launch directory when it names none, minus
/// the content the session skipped.
pub(crate) fn repository_set(
    home: &Path,
    project: &config::ProjectKey,
    launch: &Path,
    scope: &RepositoryScope,
    clock: &Arc<dyn contract::clock::Clock>,
) -> extensions::RepositorySet {
    let workspace = scope
        .workspace
        .clone()
        .unwrap_or_else(|| launch.to_path_buf());
    let skipped: BTreeSet<(String, String)> = scope
        .skipped
        .iter()
        .map(|key| (key.name.clone(), key.hash.clone()))
        .collect();
    extensions::RepositorySet::load(home, project, &workspace, &skipped, fetch(clock))
}

/// `extension_required_failed` naming the first missing required
/// extension and its notice's text, then `hook_failed` naming the first
/// approved required hook whose pinned copy could not be made.
pub(crate) fn required(
    extensions: &extensions::SessionExtensions,
) -> Result<(), contract::shapes::Failure> {
    if let Some(name) = extensions.missing_required().into_iter().next() {
        let short = config::short_name(&name);
        let detail = extensions
            .notices()
            .iter()
            .find(|notice| notice.extension.as_deref() == Some(name.as_str()))
            .map(|notice| notice.message.clone())
            .unwrap_or_else(|| "it did not start".to_owned());
        return Err(crate::failed(
            contract::ErrorCode::ExtensionRequiredFailed,
            format!(
                "The repository requires the extension `{short}`, which did not load: {detail}"
            ),
        ));
    }
    if let Some((name, detail)) = extensions.first_hook_required_failure() {
        return Err(crate::failed(
            contract::ErrorCode::HookFailed,
            format!("The repository requires the hook `{name}`, which did not run: {detail}"),
        ));
    }
    Ok(())
}

/// `extension_missing` naming `fiber extension install hooks` when declared
/// hooks have no extension to run them and any entry is `blocking` (or its
/// `on_failure` is missing or invalid), so a guard never fails
/// open (`docs/extensions.md`, "Hooks declared in configuration"). A
/// settings read that fails is not a missing extension: the read's error
/// (`config_invalid`, `io_failed`) fails the start instead.
pub(crate) fn missing_hooks(
    config: &config::Config,
    extensions: &extensions::SessionExtensions,
) -> Result<(), contract::shapes::Failure> {
    use crate::failed;

    let loaded = extensions.loaded_names();
    let filtered = extensions.hooks_config(config);
    let state = match extensions::hooks_state(&filtered, &loaded) {
        Ok(state) => state,
        Err(error) => return Err(failed(error.code(), error)),
    };
    if !matches!(state, extensions::HooksState::MissingBlocking) {
        return Ok(());
    }
    Err(failed(
        contract::ErrorCode::ExtensionMissing,
        "The `hooks` extension is not installed. Run `fiber extension install hooks`.",
    ))
}

/// Registers the session's one watcher fan-out on `log`: every event
/// appended from now on — durable and ephemeral, from every source — is
/// offered to the current set's `deliver` with its seq, in the one place
/// all session lines pass through. The offer resolves the set per event,
/// so after a reload's `swap` the fan-out reaches the reloaded set with
/// no second subscription and no ordering gap. While a shutdown code is
/// set the offers skip: when a signal stops Fiber, no watcher runs.
/// Records the log end as the watermark first, so the drain defaults
/// unset cursors to the post-registration range. Dropping the returned
/// guard unregisters.
pub(crate) fn subscribe(
    log: &Arc<Log>,
    current: &Arc<Current>,
    cancel: &Arc<r#loop::TurnCancel>,
) -> log::Subscription {
    current.get().set_watermark(log.count());
    let current = Arc::clone(current);
    let cancel = Arc::clone(cancel);
    log.subscribe(Arc::new(move |event, line| {
        if cancel.shutdown_code().is_some() {
            return;
        }
        current.get().deliver(event, line.seq);
    }))
}

/// Writes `extensions_loaded` and the notices loading raised.
pub(crate) fn written(log: &Log, extensions: &SessionExtensions) -> Result<(), r#loop::Error> {
    r#loop::extensions_loaded(log, extensions.loaded(), extensions.notices())
}

/// Every notice the startup already wrote: the session's extension
/// notices, plus the startup notices `mcp_servers_started` writes
/// after the late preparer reads them. The late preparer drops every
/// notice equal to one of these, so `config_key_ignored` and the like
/// reach the late turn once.
pub(crate) fn startup_written(
    extensions: &SessionExtensions,
    startup: &[contract::events::Notice],
) -> Vec<contract::events::Notice> {
    let mut written = extensions.notices();
    written.extend(startup.iter().cloned());
    written
}

/// The startup notices `mcp_servers_started` still has to write: the
/// providers load and the session extensions load share `candidates`,
/// so a shadow report lands in both lists, and `written` already
/// wrote the session's half.
pub(crate) fn unwritten(
    notices: Vec<contract::events::Notice>,
    extensions: &SessionExtensions,
) -> Vec<contract::events::Notice> {
    let written = extensions.notices();
    notices
        .into_iter()
        .filter(|notice| !written.contains(notice))
        .collect()
}

/// How the session's repository approvals fetch dependencies: the github
/// `Origin`, this binary's version, on `clock`. The repository set load
/// takes it.
pub(crate) fn fetch(clock: &Arc<dyn contract::clock::Clock>) -> extensions::Fetch {
    extensions::Fetch::new(
        extensions::Origin::github(),
        env!("CARGO_PKG_VERSION"),
        Arc::clone(clock),
    )
}

/// Releases the normal end's extension-owned ids through the current set:
/// the loop forgets those bindings at its top, so their completions queue
/// no `run`. Resolves the set per call, so a reload between the wiring
/// and the end still releases through the reloaded set.
pub(crate) fn released(looped: Loop, current: Arc<Current>) -> Loop {
    looped.released(Arc::new(move |ids: &[contract::JobId]| {
        current.get().release_delegates(ids);
    }))
}

/// Attaches loaded extensions, including empty hook chains whose state
/// still needs to reach the loop's drains.
pub(crate) fn hooked(looped: Loop, current: &Current) -> Loop {
    let extensions = current.get();
    if !extensions.has_extensions() {
        return looped;
    }
    let hooks: Arc<dyn Hooks> = extensions as Arc<dyn Hooks>;
    looped.hooks(hooks)
}

#[cfg(test)]
mod tests {
    //! The watcher fan-out subscription: everything appended reaches the
    //! watchers, unless a shutdown is in progress.

    #![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

    use std::sync::Arc;
    use std::time::Duration;

    use contract::ErrorCode;
    use contract::events::{Event, Notice};
    use fakes::Deadline;

    use super::{Current, subscribe};
    use crate::test_support::{Rig, install_extension, load};

    /// One `host.status` line per watcher run, sent on a channel: the
    /// tests wait on the channel through a deadline, never on a sleep.
    struct StatusSignal(std::sync::mpsc::Sender<String>);

    impl contract::emit::Emit for StatusSignal {
        fn emit(&self, event: &contract::events::Event) {
            if let contract::events::Event::ExtensionUi(ui) = event
                && let contract::events::Ui::Status { status } = &ui.ui
            {
                let _ignored = self.0.send(status.clone());
            }
        }
    }

    /// A session with one watcher on `notice`: the rig and the session,
    /// each alive for the test. `prefix` names the watcher's statuses.
    fn loaded_set(name: &str, prefix: &str) -> (Rig, Arc<extensions::SessionExtensions>) {
        let rig = Rig::new("fiber-watch-subscribe");
        install_extension(
            &rig.home,
            &format!("extensions/{name}"),
            serde_json::json!({"name": format!("fiber.test/{name}"), "version": "v0.0.0", "fiber": "0.1.0", "api": 1}),
            &[],
        );
        std::fs::write(
            rig.home.join(format!("extensions/{name}/init.lua")),
            format!("fiber.watch({{\"notice\"}}, {{ timeout = 2000, run = function(ev) host.status('{prefix}:' .. ev.payload.message) end }})\n"),
        )
        .unwrap();
        let config = load(&rig.home, &rig.workspace, "test", &[] as &[&str]);
        let extensions = Arc::new(extensions::SessionExtensions::load(
            &rig.home,
            &config,
            fakes::clock::FakeClock::new(),
            Arc::new(tools::PathLocks::new()),
            None,
        ));
        (rig, extensions)
    }

    /// A session with one watcher on `notice`, subscribed on a real log:
    /// the rig, the session and the log, each alive for the test. Statuses
    /// arrive on the returned channel in run order.
    fn subscribed() -> (
        Rig,
        Arc<extensions::SessionExtensions>,
        Arc<log::Log>,
        std::sync::mpsc::Receiver<String>,
    ) {
        let (rig, extensions) = loaded_set("watch", "watch");
        assert!(
            extensions.notices().is_empty(),
            "{:?}",
            extensions.notices()
        );
        let sessions = rig.root.path().join("sessions");
        let log = Arc::new(
            log::Log::create(
                &sessions,
                contract::SessionId("s_1".into()),
                fakes::clock::FakeClock::new(),
            )
            .unwrap(),
        );
        let (status_tx, status_rx) = std::sync::mpsc::channel();
        extensions.emit_to(Arc::new(StatusSignal(status_tx)));
        (rig, extensions, log, status_rx)
    }

    fn noticed(message: &str) -> Event {
        Event::Notice(Notice {
            code: ErrorCode::ExtensionFailed,
            message: message.to_owned(),
            extension: None,
        })
    }

    #[test]
    fn session_extensions_subscribe_fans_every_append_out_to_the_watchers() {
        let (_rig, extensions, log, status_rx) = subscribed();
        let cancel = Arc::new(r#loop::TurnCancel::default());
        let current = Arc::new(Current::new(extensions));
        let _subscription = subscribe(&log, &current, &cancel);
        log.append(&noticed("hello"), None, None).unwrap();
        assert_eq!(
            Deadline::after(Duration::from_secs(5))
                .recv(&status_rx)
                .expect("waited for the watcher to run"),
            "watch:hello"
        );
    }

    #[test]
    fn session_extensions_subscribe_skips_offers_while_a_shutdown_is_set() {
        let (_rig, extensions, log, status_rx) = subscribed();
        let cancel = Arc::new(r#loop::TurnCancel::default());
        cancel.shutdown(15);
        let current = Arc::new(Current::new(extensions));
        let _subscription = subscribe(&log, &current, &cancel);
        log.append(&noticed("skipped"), None, None).unwrap();
        // The offer closure runs synchronously inside `append`: it queued
        // nothing, so no worker can run it later and the channel stays
        // empty without waiting, while the positive control above proves
        // the rig delivers in milliseconds when no shutdown is set.
        assert!(
            status_rx.try_recv().is_err(),
            "no watcher runs while a shutdown is set"
        );
    }

    #[test]
    fn session_extensions_subscribe_follows_the_swap_to_the_reloaded_set() {
        let (_rig, extensions, log, status_rx) = subscribed();
        let cancel = Arc::new(r#loop::TurnCancel::default());
        let current = Arc::new(Current::new(extensions));
        let _subscription = subscribe(&log, &current, &cancel);
        // Pre-swap positive control: the first set's watcher runs.
        log.append(&noticed("before"), None, None).unwrap();
        assert_eq!(
            Deadline::after(Duration::from_secs(5))
                .recv(&status_rx)
                .expect("waited for the first watcher to run"),
            "watch:before"
        );
        // A reload swaps in a set whose extension watches the same kind.
        let (_rig2, reloaded) = loaded_set("watch2", "watch2");
        let (status_tx, reloaded_rx) = std::sync::mpsc::channel();
        reloaded.emit_to(Arc::new(StatusSignal(status_tx)));
        current.swap(reloaded);
        log.append(&noticed("after"), None, None).unwrap();
        assert_eq!(
            Deadline::after(Duration::from_secs(5))
                .recv(&reloaded_rx)
                .expect("waited for the reloaded watcher to run"),
            "watch2:after"
        );
        // The offer resolves the set per event: the swapped-out set's
        // watcher never runs after the swap, without waiting, while the
        // reload above proves the rig delivers in milliseconds. A closure
        // that captured the first set would leave the new channel empty.
        assert!(
            status_rx.try_recv().is_err(),
            "the swapped-out set runs no watcher after the swap"
        );
    }
}

#[cfg(test)]
mod hooks_gate_tests {
    //! `missing_hooks` fails the start closed when declared hooks have no
    //! extension to run them and any entry is `blocking` (or its `on_failure`
    //! is missing or invalid); a settings read that fails carries its own
    //! error instead.

    #![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

    use super::missing_hooks;
    use crate::test_support::{Rig, load};
    use std::sync::Arc;

    fn session_extensions(rig: &Rig, config: &config::Config) -> extensions::SessionExtensions {
        extensions::SessionExtensions::load(
            &rig.home,
            config,
            fakes::clock::FakeClock::new(),
            Arc::new(tools::PathLocks::new()),
            None,
        )
    }

    #[test]
    fn missing_hooks_reports_malformed_settings_as_config_invalid() {
        let rig = Rig::new("fiber-hooks-gate-malformed");
        let file = rig.home.join("config").join("hooks.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{not json").unwrap();
        let config = load(&rig.home, &rig.workspace, "test", &[] as &[&str]);
        let extensions = session_extensions(&rig, &config);
        // Malformed settings are not a missing extension: the read's
        // error fails the start with its own code.
        let failure = missing_hooks(&config, &extensions).unwrap_err();
        assert_eq!(failure.code, contract::ErrorCode::ConfigInvalid);
    }
}
