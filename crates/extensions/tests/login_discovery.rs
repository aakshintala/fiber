//! `docs/model-routing.md`, "Logging in": the reviewer discovery's steps 3
//! and 4 (`login_provider`, then `Providers::add_lua`) answer within their
//! deadlines when the provider's Lua does not return, so the login prompt
//! never waits on a stuck extension.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

mod common;

use fakes::MUST_SUCCEED_WITHIN;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use common::{Setup, config, write};
use contract::ErrorCode;
use contract::clock::Clock;
use extensions::{Providers, SystemBrowser, login_provider};
use fakes::clock::FakeClock;
use fakes::{Deadline, ProviderServer, Response};
use serde_json::json;

/// The extension's grace after a callback's own deadline.
const GRACE: Duration = Duration::from_secs(1);

// debt: 4 copies of this helper across the extensions test binaries, move into common/mod.rs once group 9C1 merges
/// The thread parked at `at`: exactly one thread waits there.
#[track_caller]
fn thread_parked_at(clock: &FakeClock, at: Instant) -> std::thread::ThreadId {
    let mark = clock
        .mark_parked(at, fakes::Deadline::after(MUST_SUCCEED_WITHIN))
        .unwrap_or_else(|| panic!("waited for a park at {at:?} within {MUST_SUCCEED_WITHIN:?}"));
    let threads = mark.threads();
    assert_eq!(
        threads.len(),
        1,
        "only one thread parks at {at:?}: {threads:?}"
    );
    match threads.into_iter().next() {
        Some(id) => id,
        None => panic!("one parked thread"),
    }
}

/// Where the provider's Lua stops answering.
#[derive(Clone, Copy, Debug)]
enum Stuck {
    /// The provider file's top level, while the VM registers it.
    AtLoad,
    /// `models()`, once the credential gate has let it run.
    InModels,
}

/// Installs `acme-ext` registering `acme`, with Lua that calls `host.http`
/// on `url` at the point `stuck` names.
fn install(home: &std::path::Path, url: &str, stuck: Stuck) {
    let dir = home.join("extensions/acme-ext");
    write(
        &dir.join("extension.json"),
        &json!({
            "name": "acme-ext", "version": "v1.0.0", "fiber": "0.1.0", "api": 1,
            "providers": { "acme": ["http://127.0.0.1:1/v1"] },
        })
        .to_string(),
    );
    fakes::write_record(&dir);
    write(
        &dir.join("providers/acme.json"),
        &json!({
            "name": "acme",
            "login": "browser",
            "models": [
                { "id": "m", "protocol": "openai-responses", "base_url": "http://127.0.0.1:1/v1", "context_window": 1000 },
            ],
        })
        .to_string(),
    );
    let (load, models) = match stuck {
        Stuck::AtLoad => (format!("host.http({{ url = \"{url}\" }})\n"), "return {}"),
        Stuck::InModels => (
            String::new(),
            &*format!("return host.http({{ url = \"{url}\" }}).body"),
        ),
    };
    write(
        &dir.join("providers/acme.lua"),
        &format!(
            "{load}fiber.provider(\"acme\", {{\n\
             credential = {{ timeout = 60000, run = function() return {{ token = \"t\", expires_at = 4102444800 }} end }},\n\
             models = {{ timeout = 3000, run = function() {models} end }},\n\
             }})\n"
        ),
    );
}

/// Steps 3 and 4 on a caller thread: the notices `add_lua` answers with.
/// Waits for the stuck call to reach the server and the caller to wait on
/// the fake clock, then moves the clock past the callback's deadline and
/// grace. The answer must arrive; a hang fails at the wall-clock guard.
fn discover(stuck: Stuck, wait_until: Duration) -> (Vec<(ErrorCode, Option<String>)>, Vec<String>) {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(200, "held")]).unwrap();
    server.hold();
    let home = setup.home();
    install(&home, &format!("{}/", server.url()), stuck);
    config::store_credential(
        &home,
        "acme",
        "default",
        &config::Secret::new("test".into()),
    )
    .unwrap();
    let (providers, notices) = Providers::load(&home).unwrap();
    assert!(notices.is_empty(), "{notices:?}");
    let clock = FakeClock::new();
    let asked = clock.now();
    let config = config(&setup, &[]);
    let (tx, rx) = mpsc::channel();
    let caller_clock = Arc::clone(&clock);
    std::thread::spawn(move || {
        let lua = login_provider(
            &home,
            &providers,
            "acme",
            Arc::new(SystemBrowser::default()),
            caller_clock,
        )
        .unwrap();
        let mut temp = providers.clone();
        let notices = temp.add_lua(
            "acme-ext",
            &lua,
            &config,
            &fakes::children::grouped_output,
            &|_| None,
        );
        let notices = notices
            .into_iter()
            .map(|notice| (notice.code, notice.extension))
            .collect::<Vec<_>>();
        let models = temp
            .get("acme")
            .map(|data| {
                data.models
                    .iter()
                    .map(|model| model.id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        tx.send((notices, models))
    });
    assert!(
        server.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "{stuck:?}: waited for the stuck call to reach the server"
    );
    assert!(
        clock.await_parked(
            asked + wait_until + GRACE,
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "{stuck:?}: waited for the caller to park at its grace"
    );
    // The stuck `models()` suspends its callback on the wire, so the
    // scheduler parks at the callback's deadline while the caller waits at
    // the grace; a stuck load parks only the caller, at the grace. Each
    // names the park the run actually holds.
    match stuck {
        Stuck::InModels => {
            let scheduler = thread_parked_at(&clock, asked + wait_until);
            clock.advance_when_parked(
                Some(asked + wait_until),
                Some(scheduler),
                wait_until,
                &Deadline::after(MUST_SUCCEED_WITHIN),
            );
        }
        Stuck::AtLoad => {
            clock.advance_when_parked(
                Some(asked + wait_until + GRACE),
                None,
                wait_until,
                &Deadline::after(MUST_SUCCEED_WITHIN),
            );
        }
    }
    Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "steps 3 and 4 to answer")
}

#[test]
fn a_models_call_that_never_returns_is_answered_with_a_timeout_notice() {
    let (notices, models) = discover(Stuck::InModels, Duration::from_millis(3000));
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].1.as_deref(), Some("acme-ext"));
    // A `models()` that fails leaves the data file's models.
    assert_eq!(models, vec!["m".to_owned()], "{models:?}");
}

#[test]
fn a_provider_file_that_never_finishes_loading_is_answered_with_no_models() {
    // The load deadline is 2 s, with the same grace.
    // The stored login opens the presence gate, so discovery reaches the
    // stuck file: the abandoned load answers with no models and one
    // failure notice, the established result for a credentialed user.
    let (notices, models) = discover(Stuck::AtLoad, Duration::from_secs(2));
    assert_eq!(
        notices,
        vec![(ErrorCode::ExtensionFailed, Some("acme-ext".to_owned()))],
        "{notices:?}"
    );
    // No Lua-discovered models: `m` is the fixture's static data-file
    // model, while the stuck file's `models()` would return `{}`.
    assert_eq!(models, vec!["m".to_owned()], "{models:?}");
}
