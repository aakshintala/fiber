#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::expect_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use std::fs;
use std::sync::{Arc, mpsc};
use std::time::{Duration, UNIX_EPOCH};

use contract::clock::Clock;
use fakes::Deadline;
use fakes::clock::FakeClock;

use super::*;
use crate::{LuaExtension, LuaProvider};

/// How long a test waits for one read.
const WAIT: Duration = Duration::from_secs(5);

#[track_caller]
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    match Deadline::after(WAIT).recv(&rx) {
        Ok(answer) => answer,
        Err(_) => panic!("the read did not return within {WAIT:?}"),
    }
}

/// A provider `p` whose `quota()` returns `run`, on `clock`.
fn quota_provider_on(root: &fakes::TempDir, run: &str, clock: Arc<FakeClock>) -> Arc<LuaProvider> {
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("init.lua"),
        format!(
            "fiber.provider(\"p\", {{ quota = {{ timeout = 60000, run = function() return {run} end }} }})"
        ),
    )
    .unwrap();
    let extension = Arc::new(LuaExtension::new("ext", dir, home, clock));
    LuaProvider::new(extension, "p")
}

/// A provider `p` from the entry script `script`, on a fresh clock.
fn verbatim_provider(root: &fakes::TempDir, script: &str) -> Arc<LuaProvider> {
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("init.lua"), script).unwrap();
    let extension = Arc::new(LuaExtension::new("ext", dir, home, FakeClock::new()));
    LuaProvider::new(extension, "p")
}

/// A provider `p` whose `quota()` returns `run`, and its clock.
fn quota_provider(root: &fakes::TempDir, run: &str) -> (Arc<LuaProvider>, Arc<FakeClock>) {
    let clock = FakeClock::new();
    let provider = quota_provider_on(root, run, Arc::clone(&clock));
    (provider, clock)
}

fn pair() -> CredentialPair {
    pair_with("work")
}

fn pair_with(label: &str) -> CredentialPair {
    CredentialPair {
        credential: "codex".to_owned(),
        label: label.to_owned(),
    }
}

#[track_caller]
fn read(provider: &Arc<LuaProvider>) -> QuotaEntry {
    let provider = Arc::clone(provider);
    let pair = pair();
    within(move || provider.quota(&pair))
}

fn percent(name: &str, used: f64, resets_at: Option<u64>) -> Quota {
    Quota::Windows(vec![Window {
        name: name.to_owned(),
        used: WindowUse::Percent {
            used,
            resets_at: resets_at.map(|secs| UNIX_EPOCH + Duration::from_secs(secs)),
        },
    }])
}

#[test]
fn quota_return_shapes_read_through_the_public_reader() {
    let unreachable = QuotaEntry::Unreachable { last: None };
    let rows: Vec<(&str, QuotaEntry)> = vec![
        ("nil", QuotaEntry::NoQuota),
        (
            "{ windows = {} }",
            QuotaEntry::Reported(Quota::Windows(Vec::new())),
        ),
        (
            "{ windows = { { name = \"primary\", used_percent = 42.5, resets_at = 1760000000 } } }",
            QuotaEntry::Reported(percent("primary", 42.5, Some(1_760_000_000))),
        ),
        (
            "{ windows = { { name = \"primary\", used_percent = 7 } } }",
            QuotaEntry::Reported(percent("primary", 7.0, None)),
        ),
        (
            "{ windows = { { name = \"zero\", used_percent = 0 } } }",
            QuotaEntry::Reported(percent("zero", 0.0, None)),
        ),
        (
            "{ credit = { remaining = 9.99, limit = 10 } }",
            QuotaEntry::Reported(Quota::Credit {
                remaining: 9.99,
                limit: Some(10.0),
            }),
        ),
        (
            "{ credit = { remaining = 3 } }",
            QuotaEntry::Reported(Quota::Credit {
                remaining: 3.0,
                limit: None,
            }),
        ),
        // Refusals: each reads `unreachable` with no last value.
        (
            "{ windows = { { name = \"a\", used_percent = 1 } }, credit = { remaining = 1 } }",
            unreachable.clone(),
        ),
        ("{ foo = 1 }", unreachable.clone()),
        ("{}", unreachable.clone()),
        ("\"x\"", unreachable.clone()),
        (
            "{ windows = { { used_percent = 1 } } }",
            unreachable.clone(),
        ),
        (
            "{ windows = { { name = \"\", used_percent = 1 } } }",
            unreachable.clone(),
        ),
        (
            "{ windows = { { name = \"a\", used_percent = \"high\" } } }",
            unreachable.clone(),
        ),
        (
            "{ windows = { { name = \"a\", used_percent = -1 } } }",
            unreachable.clone(),
        ),
        ("{ credit = { remaining = \"lots\" } }", unreachable.clone()),
        (
            "{ credit = { remaining = 1, limit = -2 } }",
            unreachable.clone(),
        ),
        (
            "{ windows = { { name = \"a\", used_percent = 1, resets_at = \"soon\" } } }",
            unreachable.clone(),
        ),
    ];
    for (run, want) in rows {
        let root = fakes::TempDir::new("fiber-quota-shapes");
        let (provider, _) = quota_provider(&root, run);
        assert_eq!(read(&provider), want, "{run}");
    }
}

/// Seeds `pair`'s cache with `fetched` and a flight, for the transition
/// tests below.
fn seed(
    provider: &Arc<LuaProvider>,
    fetched: Option<(Option<Quota>, Instant)>,
    flight: Option<Flight>,
) {
    let pair = pair();
    let mut states = lock(&provider.quotas.state);
    let cache = states.entry(pair).or_default();
    cache.fetched = fetched;
    cache.failed = false;
    cache.flight = flight;
    cache.next_id = 8;
}

#[test]
fn an_overdue_flight_clears_and_its_late_finish_is_discarded() {
    let root = fakes::TempDir::new("fiber-quota-expire");
    let (provider, clock) = quota_provider(&root, "nil");
    let pair = pair();
    let stored = percent("primary", 42.5, None);
    let t0 = clock.now();
    seed(
        &provider,
        Some((Some(stored.clone()), t0)),
        Some(Flight { id: 7, started: t0 }),
    );
    // Past the flight's 8 s deadline: observing it clears the slot and marks
    // the fetch failed.
    clock.advance(Duration::from_secs(8));
    let now = clock.now();
    {
        let mut states = lock(&provider.quotas.state);
        let cache = states.get_mut(&pair).unwrap();
        assert!(expire_overdue(cache, now, &provider.quotas.parker));
        assert!(cache.flight.is_none());
        assert!(cache.failed);
    }
    // The abandoned fetch's late result is discarded: the stored value and
    // its fetch time are unchanged, and the entry stays `unreachable`.
    provider.finish_quota(&pair, 7, Ok(Some(percent("late", 1.0, None))));
    {
        let states = lock(&provider.quotas.state);
        let cache = states.get(&pair).unwrap();
        assert!(cache.failed);
        let Some((Some(value), at)) = &cache.fetched else {
            panic!("the late finish replaced the stored value: {cache:?}");
        };
        assert_eq!((value, *at), (&stored, t0));
        assert_eq!(
            entry(cache, clock.now(), clock.wall()),
            QuotaEntry::Unreachable {
                last: Some((stored.clone(), now.saturating_duration_since(t0))),
            }
        );
    }
    // A finish for the flight in the slot stores the value and clears the
    // failure.
    let fresh = percent("primary", 10.0, None);
    {
        let mut states = lock(&provider.quotas.state);
        let cache = states.get_mut(&pair).unwrap();
        cache.flight = Some(Flight {
            id: 8,
            started: clock.now(),
        });
    }
    provider.finish_quota(&pair, 8, Ok(Some(fresh.clone())));
    {
        let states = lock(&provider.quotas.state);
        let cache = states.get(&pair).unwrap();
        assert!(!cache.failed);
        assert!(cache.flight.is_none());
        let Some((Some(value), _)) = &cache.fetched else {
            panic!("the current finish stored nothing: {cache:?}");
        };
        assert_eq!(value, &fresh);
        assert_eq!(
            entry(cache, clock.now(), clock.wall()),
            QuotaEntry::Reported(fresh)
        );
    }
}

/// A provider `p` whose `quota()` asks `<server>/<label>` over HTTP, raising
/// on any status but 200 and returning the decoded body, on `clock`. The
/// declared timeout never ends a call during a test; the cache's own 8 s
/// cap does.
fn http_quota_provider(
    root: &fakes::TempDir,
    server: &str,
    clock: Arc<FakeClock>,
) -> Arc<LuaProvider> {
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("init.lua"),
        format!(
            "fiber.provider(\"p\", {{ quota = {{ timeout = 60000, run = function(a)\n\
             local reply = host.http({{ url = \"{server}/\" .. a.label }})\n\
             if reply.status ~= 200 then error(\"denied \" .. reply.status) end\n\
             return json.decode(reply.body)\n\
             end }} }})"
        ),
    )
    .unwrap();
    let extension = Arc::new(LuaExtension::new("ext", dir, home, clock));
    LuaProvider::new(extension, "p")
}

fn windows_body(name: &str, used: f64, resets_at: Option<u64>) -> Vec<u8> {
    let resets = resets_at.map(|secs| format!(", \"resets_at\": {secs}"));
    format!(
        "{{\"windows\": [{{\"name\": \"{name}\", \"used_percent\": {used}{}}}]}}",
        resets.unwrap_or_default()
    )
    .into_bytes()
}

fn ok(body: Vec<u8>) -> fakes::Response {
    fakes::Response::status(200, body)
}

fn denied() -> fakes::Response {
    fakes::Response::status(500, "{\"error\": \"denied\"}")
}

#[track_caller]
fn read_label(provider: &Arc<LuaProvider>, label: &str) -> QuotaEntry {
    let provider = Arc::clone(provider);
    let pair = pair_with(label);
    within(move || provider.quota(&pair))
}

#[test]
fn a_package_without_quota_reports_no_quota_and_fetches_nothing() {
    let root = fakes::TempDir::new("fiber-quota-absent");
    let server = fakes::ProviderServer::start([]).unwrap();
    let script =
        "fiber.provider(\"p\", { sign = { timeout = 1000, run = function() return {} end } })\n";
    let bare = verbatim_provider(&root, script);
    assert_eq!(read(&bare), QuotaEntry::NoQuota);
    assert!(
        server.requests().is_empty(),
        "a package without quota() fetched: {:?}",
        server.requests()
    );
}

#[test]
fn each_label_fetches_its_own_account() {
    let root = fakes::TempDir::new("fiber-quota-labels");
    let server = fakes::ProviderServer::start([
        ok(windows_body("primary", 42.5, None)),
        ok(b"{\"credit\": {\"remaining\": 9.99}}".to_vec()),
    ])
    .unwrap();
    let clock = FakeClock::new();
    let provider = http_quota_provider(&root, &server.url(), clock);
    // A reader on an empty cache fetches: nothing fetched yet is due.
    assert_eq!(
        read_label(&provider, "a"),
        QuotaEntry::Reported(percent("primary", 42.5, None))
    );
    assert_eq!(
        read_label(&provider, "b"),
        QuotaEntry::Reported(Quota::Credit {
            remaining: 9.99,
            limit: None,
        })
    );
    let paths: Vec<String> = server
        .requests()
        .iter()
        .map(|request| request.path.clone())
        .collect();
    assert_eq!(paths, ["/a", "/b"]);
}

#[test]
fn concurrent_readers_share_one_fetch() {
    let root = fakes::TempDir::new("fiber-quota-shared");
    let server = fakes::ProviderServer::start([ok(windows_body("primary", 10.0, None))]).unwrap();
    server.hold();
    let clock = FakeClock::new();
    let provider = http_quota_provider(&root, &server.url(), Arc::clone(&clock));
    let deadline = clock.now() + Duration::from_secs(8);
    let (tx, rx) = mpsc::channel();
    for _ in 0..2 {
        let provider = Arc::clone(&provider);
        let tx = tx.clone();
        std::thread::spawn(move || tx.send(provider.quota(&pair())));
    }
    drop(tx);
    assert!(
        server.await_requests(1, WAIT),
        "the shared fetch never reached the server"
    );
    assert!(
        clock.await_parked_count(deadline, 2, WAIT),
        "both readers never parked on the shared fetch"
    );
    server.release();
    let mut found: Vec<QuotaEntry> = Vec::new();
    while let Ok(entry) = Deadline::after(WAIT).recv(&rx) {
        found.push(entry);
    }
    let want = QuotaEntry::Reported(percent("primary", 10.0, None));
    assert_eq!(found, [want.clone(), want]);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_fetch_times_out_at_exactly_eight_seconds() {
    let root = fakes::TempDir::new("fiber-quota-timeout");
    let stored = windows_body("primary", 42.5, None);
    let server = fakes::ProviderServer::start([ok(stored)]).unwrap();
    let clock = FakeClock::new();
    let provider = http_quota_provider(&root, &server.url(), Arc::clone(&clock));
    let value = percent("primary", 42.5, None);
    assert_eq!(
        read(&provider),
        QuotaEntry::Reported(value.clone()),
        "the first fetch"
    );
    // Six minutes on: the cache is due, and the next fetch is held.
    clock.advance(Duration::from_secs(6 * 60));
    server.hold();
    let start = clock.now();
    let until = start + Duration::from_secs(8);
    let (tx, rx) = mpsc::channel();
    {
        let provider = Arc::clone(&provider);
        std::thread::spawn(move || tx.send(provider.quota(&pair())));
    }
    assert!(
        server.await_requests(2, WAIT),
        "the second fetch never reached the server"
    );
    assert!(
        clock.await_parked(until, WAIT),
        "the reader never parked at the fetch's deadline"
    );
    // One nanosecond short of the cap the reader is still parked: it woke,
    // re-checked, and parked again without a timed wait.
    let mark = clock.advance_marked(Duration::from_secs(8) - Duration::from_nanos(1));
    assert!(
        clock.await_parked_since(&mark, Some(until), WAIT),
        "the reader never re-parked at the same deadline"
    );
    assert!(rx.try_recv().is_err(), "the fetch ended before its 8 s cap");
    // The cap itself: `unreachable` with the last value and its age.
    clock.advance(Duration::from_nanos(1));
    assert_eq!(
        Deadline::after(WAIT)
            .recv(&rx)
            .expect("the reader never returned past the cap"),
        QuotaEntry::Unreachable {
            last: Some((value, Duration::from_secs(6 * 60 + 8))),
        }
    );
    server.release();
}

#[test]
fn a_failed_fetch_is_unreachable_with_the_last_value_and_its_age() {
    let root = fakes::TempDir::new("fiber-quota-error");
    let server = fakes::ProviderServer::start([denied()]).unwrap();
    let clock = FakeClock::new();
    let provider = http_quota_provider(&root, &server.url(), Arc::clone(&clock));
    assert_eq!(read(&provider), QuotaEntry::Unreachable { last: None });
    // After a success, a failure keeps the last value and its fetch time, so
    // the next trigger retries: the fetch time does not move.
    let root = fakes::TempDir::new("fiber-quota-error-retry");
    let value = percent("primary", 42.5, None);
    let server =
        fakes::ProviderServer::start([ok(windows_body("primary", 42.5, None)), denied()]).unwrap();
    let clock = FakeClock::new();
    let provider = http_quota_provider(&root, &server.url(), Arc::clone(&clock));
    assert_eq!(read(&provider), QuotaEntry::Reported(value.clone()));
    clock.advance(Duration::from_secs(5 * 60 + 1));
    assert_eq!(
        read(&provider),
        QuotaEntry::Unreachable {
            last: Some((value, Duration::from_secs(5 * 60 + 1))),
        }
    );
    // A bad return is a failed fetch too.
    let root = fakes::TempDir::new("fiber-quota-bad-return");
    let server = fakes::ProviderServer::start([ok(b"\"oops\"".to_vec())]).unwrap();
    let provider = http_quota_provider(&root, &server.url(), FakeClock::new());
    assert_eq!(read(&provider), QuotaEntry::Unreachable { last: None });
}

#[test]
fn a_window_reads_as_reset_once_its_reset_time_passes() {
    let root = fakes::TempDir::new("fiber-quota-reset");
    // The fake wall starts at 1_700_000_000: the window resets 60 s after it.
    let resets_at = 1_700_000_000 + 60;
    let server =
        fakes::ProviderServer::start([ok(windows_body("primary", 42.5, Some(resets_at)))]).unwrap();
    let clock = FakeClock::new();
    let provider = http_quota_provider(&root, &server.url(), Arc::clone(&clock));
    let window = || Window {
        name: "primary".to_owned(),
        used: WindowUse::Percent {
            used: 42.5,
            resets_at: Some(UNIX_EPOCH + Duration::from_secs(resets_at)),
        },
    };
    assert_eq!(
        read(&provider),
        QuotaEntry::Reported(Quota::Windows(vec![window()])),
        "before the reset"
    );
    clock.advance(Duration::from_secs(60) - Duration::from_nanos(1));
    assert_eq!(
        read(&provider),
        QuotaEntry::Reported(Quota::Windows(vec![window()])),
        "one nanosecond before the reset"
    );
    // The reset instant itself: the stored value is untouched, only the read
    // maps the window, and the cache is still fresh, so nothing refetches.
    clock.advance(Duration::from_nanos(1));
    assert_eq!(
        read(&provider),
        QuotaEntry::Reported(Quota::Windows(vec![Window {
            name: "primary".to_owned(),
            used: WindowUse::Reset,
        }])),
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn an_unreachable_last_value_maps_a_passed_reset_time() {
    let root = fakes::TempDir::new("fiber-quota-reset-unreachable");
    let resets_at = 1_700_000_000 + 60;
    let server = fakes::ProviderServer::start([
        ok(windows_body("primary", 42.5, Some(resets_at))),
        denied(),
    ])
    .unwrap();
    let clock = FakeClock::new();
    let provider = http_quota_provider(&root, &server.url(), Arc::clone(&clock));
    assert!(matches!(read(&provider), QuotaEntry::Reported(_)));
    // Past the reset and past freshness: the retry fails, and the last value
    // it carries shows the window as reset.
    clock.advance(Duration::from_secs(6 * 60));
    assert_eq!(
        read(&provider),
        QuotaEntry::Unreachable {
            last: Some((
                Quota::Windows(vec![Window {
                    name: "primary".to_owned(),
                    used: WindowUse::Reset,
                }]),
                Duration::from_secs(6 * 60),
            )),
        }
    );
}

/// A provider `p` whose `quota()` asks `<server>/<label>` and whose `sign()`
/// returns no headers, for the off-the-request-path test.
fn http_quota_sign_provider(root: &fakes::TempDir, server: &str) -> Arc<LuaProvider> {
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("init.lua"),
        format!(
            "fiber.provider(\"p\", {{\n\
             quota = {{ timeout = 60000, run = function(a)\n\
             local reply = host.http({{ url = \"{server}/\" .. a.label }})\n\
             if reply.status ~= 200 then error(\"denied \" .. reply.status) end\n\
             return json.decode(reply.body)\n\
             end }},\n\
             sign = {{ timeout = 60000, run = function() return {{}} end }},\n\
             }})"
        ),
    )
    .unwrap();
    let extension = Arc::new(LuaExtension::new("ext", dir, home, FakeClock::new()));
    LuaProvider::new(extension, "p")
}

#[track_caller]
fn quoted(
    lua: &Arc<LuaProvider>,
    inner: Arc<crate::test_support::Inner>,
) -> Arc<dyn contract::provider::Provider> {
    let lua = Arc::clone(lua);
    let inner = inner as Arc<dyn contract::provider::Provider>;
    let pair = pair();
    within(move || lua.with_quota(inner, pair)).unwrap()
}

#[test]
fn with_quota_without_quota_is_the_inner_provider_itself() {
    let root = fakes::TempDir::new("fiber-quota-unquoted");
    let script =
        "fiber.provider(\"p\", { sign = { timeout = 1000, run = function() return {} end } })\n";
    let lua = verbatim_provider(&root, script);
    let inner: Arc<dyn contract::provider::Provider> =
        Arc::new(crate::test_support::Inner::default());
    let wrapped = {
        let inner = Arc::clone(&inner);
        let pair = pair();
        within(move || lua.with_quota(inner, pair)).unwrap()
    };
    assert!(std::ptr::addr_eq(
        Arc::as_ptr(&wrapped),
        Arc::as_ptr(&inner)
    ));
}

#[test]
fn a_model_call_fetches_off_the_request_path_without_waiting() {
    let root = fakes::TempDir::new("fiber-quota-trigger");
    let server = fakes::ProviderServer::start([ok(windows_body("primary", 10.0, None))]).unwrap();
    server.hold();
    let clock = FakeClock::new();
    let lua = http_quota_provider(&root, &server.url(), clock);
    let inner = Arc::new(crate::test_support::Inner::default());
    let wrapped = quoted(&lua, Arc::clone(&inner));
    let _call = within({
        let wrapped = Arc::clone(&wrapped);
        move || wrapped.call(&crate::test_support::request())
    });
    assert_eq!(
        inner.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the call reached the inner provider"
    );
    assert!(
        server.await_requests(1, WAIT),
        "the model call never started the fetch"
    );
    server.release();
}

#[test]
fn a_model_call_refetches_only_past_five_minutes() {
    let root = fakes::TempDir::new("fiber-quota-trigger-fresh");
    let server = fakes::ProviderServer::start([
        ok(windows_body("primary", 10.0, None)),
        ok(windows_body("primary", 11.0, None)),
    ])
    .unwrap();
    let clock = FakeClock::new();
    let lua = http_quota_provider(&root, &server.url(), Arc::clone(&clock));
    let inner = Arc::new(crate::test_support::Inner::default());
    let wrapped = quoted(&lua, inner);
    let _call = wrapped.call(&crate::test_support::request());
    assert_eq!(
        read(&lua),
        QuotaEntry::Reported(percent("primary", 10.0, None)),
        "the first fetch"
    );
    // Exactly five minutes old is fresh: neither the call nor a reader
    // refetches.
    clock.advance(Duration::from_secs(5 * 60));
    let _call = wrapped.call(&crate::test_support::request());
    assert_eq!(
        read(&lua),
        QuotaEntry::Reported(percent("primary", 10.0, None))
    );
    assert_eq!(server.requests().len(), 1);
    // One nanosecond more, and the model call starts the refetch itself.
    clock.advance(Duration::from_nanos(1));
    let _call = wrapped.call(&crate::test_support::request());
    assert!(
        server.await_requests(2, WAIT),
        "the model call never started the refetch"
    );
    assert_eq!(
        read(&lua),
        QuotaEntry::Reported(percent("primary", 11.0, None))
    );
}

#[test]
fn quoted_forwards_calls_tools_warms_and_cost_lookup() {
    let root = fakes::TempDir::new("fiber-quota-forward");
    let server = fakes::ProviderServer::start([ok(windows_body("primary", 10.0, None))]).unwrap();
    let lua = http_quota_provider(&root, &server.url(), FakeClock::new());
    let inner = Arc::new(crate::test_support::Inner {
        calls: std::sync::atomic::AtomicUsize::new(0),
        warms: true,
    });
    let wrapped = quoted(&lua, Arc::clone(&inner));
    let tools = [contract::provider::ToolDefinition {
        name: "b".into(),
        description: String::new(),
        input_schema: serde_json::json!({"type": "object"}),
        deferred: false,
        hosted: None,
    }];
    assert_eq!(wrapped.wire_tools(&tools), inner.wire_tools(&tools));
    assert!(wrapped.warms(&crate::test_support::request()));
    assert!(wrapped.cost_lookup().is_none());
    let _call = wrapped.call(&crate::test_support::request());
    assert_eq!(inner.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn a_failed_fetch_never_reaches_the_caller() {
    let root = fakes::TempDir::new("fiber-quota-trigger-error");
    let server = fakes::ProviderServer::start([denied()]).unwrap();
    let lua = http_quota_provider(&root, &server.url(), FakeClock::new());
    let inner = Arc::new(crate::test_support::Inner::default());
    let wrapped = quoted(&lua, Arc::clone(&inner));
    let _call = within({
        let wrapped = Arc::clone(&wrapped);
        move || wrapped.call(&crate::test_support::request())
    });
    assert_eq!(inner.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(read(&lua), QuotaEntry::Unreachable { last: None });
}

#[test]
fn sign_runs_while_a_quota_fetch_is_held() {
    use contract::signing::Signer;
    let root = fakes::TempDir::new("fiber-quota-off-path");
    let server = fakes::ProviderServer::start([ok(windows_body("primary", 10.0, None))]).unwrap();
    server.hold();
    let lua = http_quota_sign_provider(&root, &server.url());
    let inner = Arc::new(crate::test_support::Inner::default());
    let wrapped = quoted(&lua, inner);
    let _call = wrapped.call(&crate::test_support::request());
    assert!(
        server.await_requests(1, WAIT),
        "the quota fetch never reached the server"
    );
    // The fetch is parked on `host.http`, which does not hold the extension's
    // thread: `sign()` on the request path still runs.
    let found = within({
        let lua = Arc::clone(&lua);
        move || {
            lua.sign(&contract::signing::SignRequest {
                method: "POST",
                url: "http://127.0.0.1:9/v1",
                headers: &[],
                body: b"{}",
            })
        }
    });
    assert!(found.is_ok(), "sign never returned: {found:?}");
    server.release();
}
