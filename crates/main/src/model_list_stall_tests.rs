//! The real model-list reader's cached result for an installed static provider.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixture")]

use crate::model_list::reader;
use crate::switch::Reads;
use crate::test_support::{Rig, install_extension};
use contract::clock::Clock;
use fakes::{Deadline, clock::FakeClock};
use serde_json::json;
use std::sync::{Arc, mpsc};
use std::time::Duration;

const DEADLINE: Duration = Duration::from_secs(10);

/// A static installed provider with a cached list and a stored credential.
fn installed_reader() -> (Rig, Arc<FakeClock>, tui::ReadModels) {
    let rig = Rig::new("fiber-model-list-stall");
    install_extension(
        &rig.home,
        "extensions/fake",
        json!({"name":"fake", "version":"v1.0.0", "fiber":"0.0.0", "api":extensions::API}),
        &[(
            "fake",
            json!({"name":"fake", "models":[
                {"id":"alpha-1", "protocol":"openai-responses", "base_url":"http://127.0.0.1:1/v1", "context_window":1000},
                {"id":"beta-2", "protocol":"openai-responses", "base_url":"http://127.0.0.1:1/v1", "context_window":1000}
            ]}),
        )],
    );
    config::store_credential(
        &rig.home,
        "fake",
        "default",
        &config::Secret::new("test".into()),
    )
    .unwrap();
    config::write_model_cache(
        &rig.home,
        "fake",
        &json!([
            {"id":"alpha-1", "protocol":"openai-responses", "base_url":"http://127.0.0.1:1/v1", "context_window":1000},
            {"id":"beta-2", "protocol":"openai-responses", "base_url":"http://127.0.0.1:1/v1", "context_window":1000}
        ]),
    )
    .unwrap();
    let clock = FakeClock::new();
    let clock_trait: Arc<dyn Clock> = clock.clone();
    let read = reader(
        rig.home.clone(),
        rig.workspace.clone(),
        clock_trait,
        Arc::new(tools::PathLocks::new()),
        Arc::new(Reads::default()),
        None,
    );
    (rig, clock, read)
}

#[test]
fn installed_provider_cached_reader_returns_both_models() {
    let (_rig, _clock, read) = installed_reader();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("real-model-list-reader-test".to_owned())
        .spawn(move || {
            entered_tx.send(()).unwrap_or(());
            let _ = release_rx.recv();
            result_tx.send(read(tui::Refresh::Cached)).unwrap_or(());
        })
        .expect("reader test thread starts");
    Deadline::after(DEADLINE)
        .recv(&entered_rx)
        .expect("the actual reader invocation is gated at entry");
    release_tx.send(()).expect("release actual reader");
    let result = Deadline::after(DEADLINE)
        .recv(&result_rx)
        .expect("actual Cached reader returns");
    let catalogue = result.expect("the static installed-provider cache reads");
    let ids: Vec<&str> = catalogue
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(ids, ["alpha-1", "beta-2"]);
}
