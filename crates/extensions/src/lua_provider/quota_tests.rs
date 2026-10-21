#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]
#![allow(clippy::expect_used, reason = "test code; a failure is the test's")]
#![allow(clippy::panic, reason = "the test's wait deadline is its failure")]

use std::fs;
use std::sync::{Arc, mpsc};
use std::time::{Duration, UNIX_EPOCH};

use contract::GenerationId;
use contract::clock::Clock;
use contract::provider::CostLookup;
use fakes::Deadline;
use fakes::clock::FakeClock;

use super::*;
use crate::{LuaExtension, LuaProvider};

/// How long a test waits for one read.
const WAIT: Duration = Duration::from_secs(5);

/// A provider `p` whose `quota()` returns `run`, on `clock`.
fn quota_provider_on(root: &fakes::TempDir, run: &str, clock: Arc<FakeClock>) -> Arc<LuaProvider> {
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("providers/p.lua"),
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
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(dir.join("providers/p.lua"), script).unwrap();
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
    fakes::within(
        "provider.quota",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || provider.quota(&pair, None),
    )
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
    // No reader exists: seed(..) wrote the cache, and the advance only ages it (ruling 6179406086).
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

#[test]
fn an_overdue_finish_with_no_reader_is_discarded() {
    let root = fakes::TempDir::new("fiber-quota-late-finish");
    let (provider, clock) = quota_provider(&root, "nil");
    let pair = pair();
    let stored = percent("primary", 42.5, None);
    let t0 = clock.now();
    seed(
        &provider,
        Some((Some(stored.clone()), t0)),
        Some(Flight { id: 7, started: t0 }),
    );
    // No reader or trigger runs: the overdue fetch finishes on its own past
    // the 8 s cap, and the completion expires it instead of caching success.
    // No reader or trigger runs here; the advance only ages the seeded flight (ruling 6179406086).
    clock.advance(Duration::from_secs(8));
    let now = clock.now();
    provider.finish_quota(&pair, 7, Ok(Some(percent("late", 1.0, None))));
    {
        let states = lock(&provider.quotas.state);
        let cache = states.get(&pair).unwrap();
        assert!(
            cache.flight.is_none(),
            "the overdue finish kept the slot: {cache:?}"
        );
        assert!(
            cache.failed,
            "the overdue finish did not mark the fetch failed: {cache:?}"
        );
        let Some((Some(value), at)) = &cache.fetched else {
            panic!("the late finish replaced the stored value: {cache:?}");
        };
        assert_eq!((value, *at), (&stored, t0));
        assert_eq!(
            entry(cache, now, clock.wall()),
            QuotaEntry::Unreachable {
                last: Some((stored, now.saturating_duration_since(t0))),
            }
        );
    }
}

#[test]
fn finish_at_the_eight_second_boundary() {
    let root = fakes::TempDir::new("fiber-quota-finish-boundary");
    let (provider, clock) = quota_provider(&root, "nil");
    let pair = pair();
    let t0 = clock.now();
    seed(&provider, None, Some(Flight { id: 7, started: t0 }));
    // One nanosecond short of the cap the finish is accepted.
    // No reader exists; the advance only ages the seeded flight (ruling 6179406086).
    clock.advance(Duration::from_secs(8) - Duration::from_nanos(1));
    let fresh = percent("primary", 10.0, None);
    provider.finish_quota(&pair, 7, Ok(Some(fresh.clone())));
    let fetched_at = {
        let states = lock(&provider.quotas.state);
        let cache = states.get(&pair).unwrap();
        assert!(
            !cache.failed,
            "a finish at 8 s - 1 ns marked failed: {cache:?}"
        );
        assert!(cache.flight.is_none());
        let Some((Some(value), at)) = &cache.fetched else {
            panic!("a finish at 8 s - 1 ns stored nothing: {cache:?}");
        };
        assert_eq!(value, &fresh);
        *at
    };
    // At exactly the cap the finish is discarded: the last value and its
    // fetch time are unchanged, and the entry stays `unreachable`.
    {
        let mut states = lock(&provider.quotas.state);
        let cache = states.get_mut(&pair).unwrap();
        cache.flight = Some(Flight {
            id: 8,
            started: clock.now(),
        });
    }
    let started = clock.now();
    // No reader exists; the advance only ages the seeded flight (ruling 6179406086).
    clock.advance(Duration::from_secs(8));
    let now = clock.now();
    provider.finish_quota(&pair, 8, Ok(Some(percent("late", 1.0, None))));
    {
        let states = lock(&provider.quotas.state);
        let cache = states.get(&pair).unwrap();
        assert!(cache.flight.is_none());
        assert!(
            cache.failed,
            "a finish at exactly 8 s did not mark the fetch failed: {cache:?}"
        );
        let Some((Some(value), at)) = &cache.fetched else {
            panic!("the late finish replaced the stored value: {cache:?}");
        };
        assert_eq!((value, *at), (&fresh, fetched_at));
        assert_eq!(
            entry(cache, now, clock.wall()),
            QuotaEntry::Unreachable {
                last: Some((fresh, now.saturating_duration_since(fetched_at))),
            }
        );
        assert_eq!(
            now.saturating_duration_since(started),
            Duration::from_secs(8)
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
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("providers/p.lua"),
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
    fakes::within(
        "provider.quota",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || provider.quota(&pair, None),
    )
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
fn quota_now_returns_while_the_vm_is_busy_and_starts_only_one_fetch() {
    let root = fakes::TempDir::new("fiber-quota-now-held");
    let server = fakes::ProviderServer::start([ok(windows_body("primary", 10.0, None))]).unwrap();
    server.hold();
    let provider = http_quota_provider(&root, &server.url(), FakeClock::new());
    assert!(
        fakes::within("registers", Deadline::after(fakes::MUST_SUCCEED_WITHIN), {
            let provider = provider.clone();
            move || provider.registers("quota")
        })
        .unwrap()
    );
    assert_eq!(
        fakes::within("quota_now", Deadline::after(fakes::MUST_SUCCEED_WITHIN), {
            let provider = provider.clone();
            move || provider.quota_now(&pair(), None)
        }),
        QuotaEntry::Unreachable { last: None }
    );
    assert!(
        server.await_requests(1, Deadline::after(WAIT)),
        "the fetch reached the held server"
    );
    assert_eq!(
        fakes::within("quota_now", Deadline::after(fakes::MUST_SUCCEED_WITHIN), {
            let provider = provider.clone();
            move || provider.quota_now(&pair(), None)
        }),
        QuotaEntry::Unreachable { last: None }
    );
    assert_eq!(server.requests().len(), 1);
    server.release();
    assert_eq!(
        read(&provider),
        QuotaEntry::Reported(percent("primary", 10.0, None))
    );
}

#[test]
fn quota_now_refetches_only_after_the_freshness_boundary() {
    let root = fakes::TempDir::new("fiber-quota-now-fresh");
    let server = fakes::ProviderServer::start([
        ok(windows_body("primary", 10.0, None)),
        ok(windows_body("primary", 11.0, None)),
    ])
    .unwrap();
    let clock = FakeClock::new();
    let provider = http_quota_provider(&root, &server.url(), clock.clone());
    let value = QuotaEntry::Reported(percent("primary", 10.0, None));
    assert_eq!(read(&provider), value);
    for advance in [
        FRESH_FOR.checked_sub(Duration::from_nanos(1)).unwrap(),
        Duration::from_nanos(1),
    ] {
        // read(..) returned, so no reader is parked; the advance only ages the cache (ruling 6179406086).
        clock.advance(advance);
        assert_eq!(provider.quota_now(&pair(), None), value);
        assert!(
            lock(&provider.quotas.state)
                .get(&pair())
                .unwrap()
                .flight
                .is_none()
        );
    }
    // flight.is_none() is asserted above, so no fetch is in flight; the advance only ages the cache (ruling 6179406086).
    clock.advance(Duration::from_nanos(1));
    server.hold();
    assert_eq!(provider.quota_now(&pair(), None), value);
    assert!(
        server.await_requests(2, Deadline::after(WAIT)),
        "the overdue cache started a fetch"
    );
    assert_eq!(server.requests().len(), 2);
    assert_eq!(
        fakes::within("quota_now", Deadline::after(fakes::MUST_SUCCEED_WITHIN), {
            let provider = provider.clone();
            move || provider.quota_now(&pair(), None)
        }),
        value
    );
    server.release();
    assert_eq!(
        read(&provider),
        QuotaEntry::Reported(percent("primary", 11.0, None))
    );
}

#[test]
fn quota_cached_never_starts_a_fetch_even_after_the_freshness_boundary() {
    let root = fakes::TempDir::new("fiber-quota-cached");
    let server = fakes::ProviderServer::start([
        ok(windows_body("primary", 10.0, None)),
        ok(windows_body("primary", 11.0, None)),
    ])
    .unwrap();
    let clock = FakeClock::new();
    let provider = http_quota_provider(&root, &server.url(), clock.clone());
    assert_eq!(
        provider.quota_cached(&pair()),
        QuotaEntry::Unreachable { last: None }
    );
    assert!(
        lock(&provider.quotas.state)
            .get(&pair())
            .unwrap()
            .flight
            .is_none()
    );
    assert!(server.requests().is_empty());
    let value = QuotaEntry::Reported(percent("primary", 10.0, None));
    assert_eq!(read(&provider), value);
    // read(..) returned and quota_cached starts no fetch; the advance only ages the cache (ruling 6179406086).
    clock.advance(FRESH_FOR + Duration::from_nanos(1));
    assert_eq!(provider.quota_cached(&pair()), value);
    assert!(
        lock(&provider.quotas.state)
            .get(&pair())
            .unwrap()
            .flight
            .is_none()
    );
    assert_eq!(server.requests().len(), 1);
    server.hold();
    assert_eq!(provider.quota_now(&pair(), None), value);
    assert!(
        server.await_requests(2, Deadline::after(WAIT)),
        "quota_now starts the overdue fetch"
    );
    server.release();
    assert_eq!(
        read(&provider),
        QuotaEntry::Reported(percent("primary", 11.0, None))
    );
}

#[test]
fn quota_now_expires_an_overdue_flight_without_waiting() {
    let root = fakes::TempDir::new("fiber-quota-now-expire");
    let (provider, clock) = quota_provider(&root, "nil");
    let value = percent("weekly", 35.0, None);
    for age in [
        FETCH_TIMEOUT.checked_sub(Duration::from_nanos(1)).unwrap(),
        FETCH_TIMEOUT,
        FETCH_TIMEOUT + Duration::from_nanos(1),
    ] {
        let now = clock.now();
        lock(&provider.quotas.state).insert(
            pair(),
            Cache {
                fetched: Some((Some(value.clone()), now)),
                flight: Some(Flight {
                    id: 0,
                    started: now.checked_sub(age).unwrap(),
                }),
                ..Cache::default()
            },
        );
        let expected = if age < FETCH_TIMEOUT {
            QuotaEntry::Reported(value.clone())
        } else {
            QuotaEntry::Unreachable {
                last: Some((value.clone(), Duration::ZERO)),
            }
        };
        assert_eq!(
            fakes::within("quota_now", Deadline::after(fakes::MUST_SUCCEED_WITHIN), {
                let provider = provider.clone();
                move || provider.quota_now(&pair(), None)
            }),
            expected
        );
    }
    lock(&provider.quotas.state).insert(
        pair(),
        Cache {
            fetched: Some((None, clock.now())),
            ..Cache::default()
        },
    );
    assert_eq!(provider.quota_now(&pair(), None), QuotaEntry::NoQuota);
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
        std::thread::spawn(move || tx.send(provider.quota(&pair(), None)));
    }
    drop(tx);
    assert!(
        server.await_requests(1, Deadline::after(WAIT)),
        "the shared fetch never reached the server"
    );
    assert!(
        clock.await_parked_count(deadline, 2, fakes::Deadline::after(WAIT)),
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
    // The reader thread spawns below; nothing waits on the clock here yet (ruling 6179406086).
    clock.advance(Duration::from_secs(6 * 60));
    server.hold();
    let start = clock.now();
    let until = start + Duration::from_secs(8);
    let (tx, rx) = mpsc::channel();
    let reader = {
        let provider = Arc::clone(&provider);
        std::thread::spawn(move || tx.send(provider.quota(&pair(), None)))
    };
    assert!(
        server.await_requests(2, Deadline::after(WAIT)),
        "the second fetch never reached the server"
    );
    assert!(
        clock.await_parked(until, fakes::Deadline::after(WAIT)),
        "the reader never parked at the fetch's deadline"
    );
    // One nanosecond short of the cap the reader is still parked: it woke,
    // re-checked, and parked again without a timed wait.
    let mark = clock.advance_when_parked(
        Some(until),
        Some(reader.thread().id()),
        Duration::from_secs(8) - Duration::from_nanos(1),
        &Deadline::after(WAIT),
    );
    assert!(
        clock.await_parked_since(&mark, Some(until), fakes::Deadline::after(WAIT)),
        "the reader never re-parked at the same deadline"
    );
    assert!(rx.try_recv().is_err(), "the fetch ended before its 8 s cap");
    // The cap itself: `unreachable` with the last value and its age.
    clock.advance_when_parked(
        Some(until),
        Some(reader.thread().id()),
        Duration::from_nanos(1),
        &Deadline::after(WAIT),
    );
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
    // read(..) returned; the advance only ages the cache (ruling 6179406086).
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
    // read(..) returned; the advance only moves the reset boundary (ruling 6179406086).
    clock.advance(Duration::from_secs(60) - Duration::from_nanos(1));
    assert_eq!(
        read(&provider),
        QuotaEntry::Reported(Quota::Windows(vec![window()])),
        "one nanosecond before the reset"
    );
    // The reset instant itself: the stored value is untouched, only the read
    // maps the window, and the cache is still fresh, so nothing refetches.
    // read(..) returned and the cache is still fresh, so nothing waits on the clock here (ruling 6179406086).
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
    // read(..) returned; the advance only ages the cache (ruling 6179406086).
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

#[test]
fn a_reader_reports_its_own_flight_when_the_flight_is_replaced() {
    let root = fakes::TempDir::new("fiber-quota-replaced-flight");
    let (provider, clock) = quota_provider(&root, "nil");
    let pair = pair();
    let t0 = clock.now();
    let value = percent("primary", 42.5, None);
    seed(
        &provider,
        Some((Some(value.clone()), t0)),
        Some(Flight { id: 7, started: t0 }),
    );
    let (tx, rx) = mpsc::channel();
    let reader = {
        let provider = Arc::clone(&provider);
        let pair = pair.clone();
        std::thread::spawn(move || tx.send(provider.quota(&pair, None)))
    };
    assert!(
        clock.await_parked(t0 + Duration::from_secs(8), fakes::Deadline::after(WAIT)),
        "the reader never parked on flight 7"
    );
    // Hold the cache while the clock passes flight 7's deadline, so the reader
    // wakes only after flight 7 has expired and flight 8 is in the slot: the
    // reader must report on flight 7, not wait on flight 8.
    {
        let mut states = lock(&provider.quotas.state);
        clock.advance_when_parked(
            Some(t0 + Duration::from_secs(8)),
            Some(reader.thread().id()),
            Duration::from_secs(8),
            &Deadline::after(WAIT),
        );
        let now = clock.now();
        let cache = states.get_mut(&pair).unwrap();
        assert!(expire_overdue(cache, now, &provider.quotas.parker));
        let id = cache.next_id;
        cache.next_id = id.wrapping_add(1);
        cache.flight = Some(Flight { id, started: now });
    }
    match Deadline::after(WAIT).recv(&rx) {
        Ok(found) => assert_eq!(
            found,
            QuotaEntry::Unreachable {
                last: Some((value, Duration::from_secs(8))),
            },
            "the reader did not report on its own flight"
        ),
        Err(_) => panic!("the reader waited on the replacement flight past {WAIT:?}"),
    }
}

/// A provider `p` whose `quota()` asks `<server>/<label>` and whose `sign()`
/// returns no headers, for the off-the-request-path test.
fn http_quota_sign_provider(root: &fakes::TempDir, server: &str) -> Arc<LuaProvider> {
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(dir.join("providers")).unwrap();
    fs::write(
        dir.join("providers/p.lua"),
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
    fakes::within(
        "lua.with_quota",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || lua.with_quota(inner, pair, None),
    )
    .unwrap()
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
        fakes::within(
            "lua.with_quota",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || lua.with_quota(inner, pair, None),
        )
        .unwrap()
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
    let _call = fakes::within(
        "wrapped.call",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let wrapped = Arc::clone(&wrapped);
            move || wrapped.call(&crate::test_support::request())
        },
    );
    assert_eq!(
        inner.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the call reached the inner provider"
    );
    assert!(
        server.await_requests(1, Deadline::after(WAIT)),
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
    // read(..) returned and nothing refetches; the advance only ages the cache (ruling 6179406086).
    clock.advance(Duration::from_secs(5 * 60));
    let _call = wrapped.call(&crate::test_support::request());
    assert_eq!(
        read(&lua),
        QuotaEntry::Reported(percent("primary", 10.0, None))
    );
    assert_eq!(server.requests().len(), 1);
    // One nanosecond more, and the model call starts the refetch itself.
    // read(..) returned; the advance only ages the cache past freshness (ruling 6179406086).
    clock.advance(Duration::from_nanos(1));
    let _call = wrapped.call(&crate::test_support::request());
    assert!(
        server.await_requests(2, Deadline::after(WAIT)),
        "the model call never started the refetch"
    );
    assert_eq!(
        read(&lua),
        QuotaEntry::Reported(percent("primary", 11.0, None))
    );
}

/// The lookup `Priced` returns: one fixed cost, so a test can tell it arrived
/// through the wrapper.
struct FixedCost;

impl CostLookup for FixedCost {
    fn cost(&self, _generation_id: &GenerationId) -> Option<f64> {
        Some(0.25)
    }
}

/// `Inner` with a `cost_lookup`. `Inner` has none, so a forwarded `None`
/// would look the same as the wrapper's own default.
struct Priced(Arc<crate::test_support::Inner>);

impl Provider for Priced {
    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        self.0.call(request)
    }

    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        self.0.wire_tools(tools)
    }

    fn warms(&self, request: &ModelRequest) -> bool {
        self.0.warms(request)
    }

    fn cost_lookup(&self) -> Option<Arc<dyn CostLookup>> {
        Some(Arc::new(FixedCost))
    }
}

#[test]
fn quoted_forwards_calls_tools_warms_and_cost_lookup() {
    let root = fakes::TempDir::new("fiber-quota-forward");
    let server = fakes::ProviderServer::start([ok(windows_body("primary", 10.0, None))]).unwrap();
    let lua = http_quota_provider(&root, &server.url(), FakeClock::new());
    // One inner per value of `warms`: a wrapper that answered its own
    // default (`true`) or a constant would match only one of them.
    for warms in [true, false] {
        let inner = Arc::new(crate::test_support::Inner {
            calls: std::sync::atomic::AtomicUsize::new(0),
            warms,
        });
        let priced: Arc<dyn Provider> = Arc::new(Priced(Arc::clone(&inner)));
        let wrapped = {
            let lua = Arc::clone(&lua);
            let pair = pair();
            fakes::within(
                "lua.with_quota",
                Deadline::after(fakes::MUST_SUCCEED_WITHIN),
                move || lua.with_quota(priced, pair, None),
            )
            .unwrap()
        };
        let tools = [ToolDefinition {
            name: "b".into(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }];
        assert_eq!(wrapped.wire_tools(&tools), inner.wire_tools(&tools));
        assert_eq!(
            wrapped.warms(&crate::test_support::request()),
            warms,
            "warms {warms} was not forwarded"
        );
        let lookup = wrapped
            .cost_lookup()
            .expect("cost_lookup was not forwarded");
        assert_eq!(
            lookup.cost(&GenerationId("gen".to_owned())),
            Some(0.25),
            "cost_lookup was not forwarded"
        );
        let _call = wrapped.call(&crate::test_support::request());
        assert_eq!(inner.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}

#[test]
fn a_failed_fetch_never_reaches_the_caller() {
    let root = fakes::TempDir::new("fiber-quota-trigger-error");
    let server = fakes::ProviderServer::start([denied()]).unwrap();
    let lua = http_quota_provider(&root, &server.url(), FakeClock::new());
    let inner = Arc::new(crate::test_support::Inner::default());
    let wrapped = quoted(&lua, Arc::clone(&inner));
    let _call = fakes::within(
        "wrapped.call",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let wrapped = Arc::clone(&wrapped);
            move || wrapped.call(&crate::test_support::request())
        },
    );
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
        server.await_requests(1, Deadline::after(WAIT)),
        "the quota fetch never reached the server"
    );
    // The fetch is parked on `host.http`, which does not hold the extension's
    // thread: `sign()` on the request path still runs.
    let found = fakes::within("lua.sign", Deadline::after(fakes::MUST_SUCCEED_WITHIN), {
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

/// A provider `p` whose `quota()` names the label, credential and key it
/// was given, so a test can tell the argument reached it.
fn arg_echo_provider(root: &fakes::TempDir) -> Arc<LuaProvider> {
    verbatim_provider(
        root,
        "fiber.provider(\"p\", { quota = { timeout = 60000, run = function(a)\n\
         return { windows = { { name = a.label .. \"/\" .. a.credential .. \"/\" .. tostring(a.key), used_percent = 0 } } }\n\
         end } })\n",
    )
}

#[test]
fn fetch_quota_passes_label_credential_and_key() {
    let root = fakes::TempDir::new("fiber-quota-arg");
    let provider = arg_echo_provider(&root);
    let key = Secret::new("sk-test".to_owned());
    let (quota, raw) = {
        let provider = Arc::clone(&provider);
        fakes::within(
            "provider.fetch_quota",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || provider.fetch_quota(&pair(), Some(&key)),
        )
    }
    .unwrap();
    assert_eq!(
        quota,
        Some(percent("work/codex/sk-test", 0.0, None)),
        "the key did not reach quota()"
    );
    assert_eq!(
        raw["windows"][0]["name"],
        serde_json::json!("work/codex/sk-test"),
        "the raw return is not the checked value"
    );
    let (quota, raw) = {
        let provider = Arc::clone(&provider);
        fakes::within(
            "provider.fetch_quota",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || provider.fetch_quota(&pair(), None),
        )
    }
    .unwrap();
    assert_eq!(
        quota,
        Some(percent("work/codex/nil", 0.0, None)),
        "an absent key did not arrive absent"
    );
    assert_eq!(
        raw["windows"][0]["name"],
        serde_json::json!("work/codex/nil")
    );
}

#[test]
fn a_read_passes_its_key() {
    let root = fakes::TempDir::new("fiber-quota-read-key");
    let provider = arg_echo_provider(&root);
    let key = Secret::new("sk-test".to_owned());
    let found = {
        let provider = Arc::clone(&provider);
        fakes::within(
            "provider.quota",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || provider.quota(&pair(), Some(&key)),
        )
    };
    assert_eq!(
        found,
        QuotaEntry::Reported(percent("work/codex/sk-test", 0.0, None)),
        "the reader's key did not reach quota()"
    );
}

#[test]
fn a_model_call_fetches_with_the_bound_key() {
    let root = fakes::TempDir::new("fiber-quota-bound-key");
    let server = fakes::ProviderServer::start([ok(windows_body("primary", 10.0, None))]).unwrap();
    server.hold();
    let home = root.path().join("home");
    let dir = root.path().join("ext");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(dir.join("providers")).unwrap();
    let url = server.url();
    fs::write(
        dir.join("providers/p.lua"),
        format!(
            "fiber.provider(\"p\", {{ quota = {{ timeout = 60000, run = function(a)\n\
             local reply = host.http({{ url = \"{url}/\" .. a.label .. \"/\" .. tostring(a.key) }})\n\
             if reply.status ~= 200 then error(\"denied \" .. reply.status) end\n\
             return json.decode(reply.body)\n\
             end }} }})"
        ),
    )
    .unwrap();
    let clock = FakeClock::new();
    let extension = Arc::new(LuaExtension::new("ext", dir, home, clock));
    let lua = LuaProvider::new(extension, "p");
    let inner = Arc::new(crate::test_support::Inner::default());
    let key = Secret::new("sk-test".to_owned());
    let wrapped = {
        let lua = Arc::clone(&lua);
        let inner = inner as Arc<dyn contract::provider::Provider>;
        fakes::within(
            "lua.with_quota",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || lua.with_quota(inner, pair(), Some(key)),
        )
        .unwrap()
    };
    let _call = wrapped.call(&crate::test_support::request());
    assert!(
        server.await_requests(1, Deadline::after(WAIT)),
        "the model call never started the fetch"
    );
    assert_eq!(
        server.requests()[0].path,
        "/work/sk-test",
        "the bound key did not make the fetch"
    );
    server.release();
    // A reader with no key reads the bound fetch from the cache, starting
    // no fetch of its own.
    let found = {
        let lua = Arc::clone(&lua);
        fakes::within(
            "lua.quota",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || lua.quota(&pair(), None),
        )
    };
    assert_eq!(found, QuotaEntry::Reported(percent("primary", 10.0, None)));
    assert_eq!(
        server.requests().len(),
        1,
        "the read fetched with its own key instead of reading the cache"
    );
}

#[test]
fn fetch_quota_reports_a_bad_return_and_a_raise_as_errors() {
    let root = fakes::TempDir::new("fiber-quota-bad-return");
    let provider = quota_provider_on(&root, "{ credit = \"x\" }", FakeClock::new());
    let err = {
        let provider = Arc::clone(&provider);
        fakes::within(
            "provider.fetch_quota",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || provider.fetch_quota(&pair(), None),
        )
    }
    .unwrap_err();
    let Error::BadReturn { callback, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(callback, "p.quota");
    let root = fakes::TempDir::new("fiber-quota-raise");
    let provider = quota_provider_on(&root, "error(\"boom\")", FakeClock::new());
    let err = {
        let provider = Arc::clone(&provider);
        fakes::within(
            "provider.fetch_quota",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || provider.fetch_quota(&pair(), None),
        )
    }
    .unwrap_err();
    assert_eq!(err.code(), contract::ErrorCode::ExtensionFailed);
}

/// A provider `p` whose `credential()` returns a fixed token and whose
/// `quota()` names the token and account id it was given.
fn token_quota_provider(root: &fakes::TempDir) -> Arc<LuaProvider> {
    verbatim_provider(
        root,
        "fiber.provider(\"p\", {\n\
         credential = { timeout = 60000, run = function()\n\
         return { token = \"tok-1\", expires_at = 4102444800,\n\
         headers = { [\"chatgpt-account-id\"] = \"acct-1\" } }\n\
         end },\n\
         quota = { timeout = 60000, run = function(a)\n\
         return { windows = { { name = a.token .. \"/\" .. a.headers[\"chatgpt-account-id\"], used_percent = 0 } } }\n\
         end },\n\
         })\n",
    )
}

#[test]
fn fetch_quota_passes_the_current_token_and_headers() {
    let root = fakes::TempDir::new("fiber-quota-token-arg");
    let provider = token_quota_provider(&root);
    let (quota, raw) = {
        let provider = Arc::clone(&provider);
        fakes::within(
            "provider.fetch_quota",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || provider.fetch_quota(&pair(), None),
        )
    }
    .unwrap();
    assert_eq!(
        quota,
        Some(percent("tok-1/acct-1", 0.0, None)),
        "the credential() token and headers did not reach quota()"
    );
    assert_eq!(raw["windows"][0]["name"], serde_json::json!("tok-1/acct-1"));
}

#[test]
fn fetch_quota_sends_no_token_or_headers_to_a_key_provider() {
    let root = fakes::TempDir::new("fiber-quota-no-token-for-keys");
    let provider = verbatim_provider(
        &root,
        "fiber.provider(\"p\", { quota = { timeout = 60000, run = function(a)\n\
         if a.token ~= nil then error(\"quota got token \" .. tostring(a.token)) end\n\
         if a.headers ~= nil then error(\"quota got headers\") end\n\
         return { windows = {} }\n\
         end } })\n",
    );
    let key = Secret::new("sk-test".to_owned());
    let (quota, _) = {
        let provider = Arc::clone(&provider);
        fakes::within(
            "provider.fetch_quota",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || provider.fetch_quota(&pair(), Some(&key)),
        )
    }
    .unwrap();
    assert_eq!(quota, Some(Quota::Windows(Vec::new())));
}

#[test]
fn a_credential_failure_marks_the_fetch_failed() {
    let root = fakes::TempDir::new("fiber-quota-credential-failure");
    let provider = verbatim_provider(
        &root,
        "fiber.provider(\"p\", {\n\
         credential = { timeout = 60000, run = function() error(\"no login\") end },\n\
         quota = { timeout = 60000, run = function() return { windows = {} } end },\n\
         })\n",
    );
    assert_eq!(read(&provider), QuotaEntry::Unreachable { last: None });
}

/// An observed factory over `inner` recording the attempts it builds.
fn recorder(
    inner: Arc<crate::test_support::Inner>,
) -> (contract::diag::Observe, Arc<std::sync::Mutex<Vec<u32>>>) {
    let attempts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = Arc::clone(&attempts);
    let observe: contract::diag::Observe = Arc::new(move |request, attempt| {
        seen.lock().unwrap().push(attempt.get());
        inner.call(request)
    });
    (observe, attempts)
}

#[test]
fn observed_and_ordinary_calls_share_one_due_quota_refresh() {
    let root = fakes::TempDir::new("fiber-quota-observed-trigger");
    let server = fakes::ProviderServer::start([ok(windows_body("primary", 10.0, None))]).unwrap();
    let lua = http_quota_provider(&root, &server.url(), FakeClock::new());
    let inner = Arc::new(crate::test_support::Inner::default());
    let (observe, attempts) = recorder(Arc::clone(&inner));
    let (wrapped, factory) = {
        let lua = Arc::clone(&lua);
        let inner = Arc::clone(&inner) as Arc<dyn contract::provider::Provider>;
        fakes::within(
            "lua.with_quota_observed",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                lua.with_quota_observed(inner, Some(observe), pair(), None)
                    .unwrap()
            },
        )
    };
    let factory = factory.expect("a quota provider keeps the factory");
    // The ordinary call builds observed at the first attempt, through the
    // same preparation as the factory.
    let _call = wrapped.call(&crate::test_support::request());
    assert_eq!(
        read(&lua),
        QuotaEntry::Reported(percent("primary", 10.0, None)),
        "the ordinary call's trigger fetched"
    );
    let _observed = fakes::within("factory", Deadline::after(fakes::MUST_SUCCEED_WITHIN), {
        let request = crate::test_support::request();
        move || factory(&request, std::num::NonZeroU32::new(2).unwrap())
    });
    assert_eq!(
        read(&lua),
        QuotaEntry::Reported(percent("primary", 10.0, None))
    );
    assert_eq!(server.requests().len(), 1, "one due refresh for both paths");
    assert_eq!(
        *attempts.lock().unwrap(),
        vec![1, 2],
        "the ordinary call builds at the first attempt"
    );
    assert_eq!(
        inner.calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "both calls reached the inner provider"
    );
}

#[test]
fn the_observed_factory_respects_freshness_for_its_bound_pair() {
    let root = fakes::TempDir::new("fiber-quota-observed-fresh");
    let server = fakes::ProviderServer::start([
        ok(windows_body("primary", 10.0, None)),
        ok(windows_body("primary", 11.0, None)),
    ])
    .unwrap();
    let clock = FakeClock::new();
    let lua = http_quota_provider(&root, &server.url(), Arc::clone(&clock));
    let inner = Arc::new(crate::test_support::Inner::default());
    let (observe, _) = recorder(Arc::clone(&inner));
    let (_, factory) = {
        let lua = Arc::clone(&lua);
        let inner = Arc::clone(&inner) as Arc<dyn contract::provider::Provider>;
        fakes::within(
            "lua.with_quota_observed",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                lua.with_quota_observed(inner, Some(observe), pair(), None)
                    .unwrap()
            },
        )
    };
    let factory = factory.expect("a quota provider keeps the factory");
    let trigger = |attempt: u32| {
        let factory = factory.clone();
        fakes::within(
            "factory",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                factory(
                    &crate::test_support::request(),
                    std::num::NonZeroU32::new(attempt).unwrap(),
                )
            },
        )
    };
    let _observed = trigger(1);
    assert_eq!(
        read(&lua),
        QuotaEntry::Reported(percent("primary", 10.0, None)),
        "the factory's trigger fetched"
    );
    assert!(
        server.requests()[0].path.ends_with("/work"),
        "the fetch ran for the bound pair: {}",
        server.requests()[0].path
    );
    // Exactly five minutes old is fresh: the factory starts no refetch.
    // read(..) returned and nothing refetches; the advance only ages the cache (ruling 6179406086).
    clock.advance(Duration::from_secs(5 * 60));
    let _observed = trigger(2);
    assert_eq!(server.requests().len(), 1);
    // One nanosecond more, and the factory starts the refetch itself.
    // read(..) returned; the advance only ages the cache past freshness (ruling 6179406086).
    clock.advance(Duration::from_nanos(1));
    let _observed = trigger(3);
    assert!(
        server.await_requests(2, Deadline::after(WAIT)),
        "the factory never started the refetch"
    );
    assert_eq!(
        read(&lua),
        QuotaEntry::Reported(percent("primary", 11.0, None))
    );
}

#[test]
fn with_quota_observed_without_quota_keeps_both_inputs() {
    let root = fakes::TempDir::new("fiber-quota-observed-unquoted");
    let script =
        "fiber.provider(\"p\", { sign = { timeout = 1000, run = function() return {} end } })\n";
    let lua = verbatim_provider(&root, script);
    let inner: Arc<dyn contract::provider::Provider> =
        Arc::new(crate::test_support::Inner::default());
    let (observe, _) = recorder(Arc::new(crate::test_support::Inner::default()));
    let (wrapped, factory) = {
        let inner = Arc::clone(&inner);
        let observe = Arc::clone(&observe);
        fakes::within(
            "lua.with_quota_observed",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || {
                lua.with_quota_observed(inner, Some(observe), pair(), None)
                    .unwrap()
            },
        )
    };
    assert!(std::ptr::addr_eq(
        Arc::as_ptr(&wrapped),
        Arc::as_ptr(&inner)
    ));
    let factory = factory.expect("no quota keeps the factory");
    assert!(std::ptr::addr_eq(
        Arc::as_ptr(&factory),
        Arc::as_ptr(&observe)
    ));
}

#[test]
fn with_quota_observed_failure_matches_with_quota() {
    let root = fakes::TempDir::new("fiber-quota-observed-broken");
    let lua = verbatim_provider(&root, "this is not lua\n");
    let inner: Arc<dyn contract::provider::Provider> =
        Arc::new(crate::test_support::Inner::default());
    let (observe, _) = recorder(Arc::new(crate::test_support::Inner::default()));
    let plain = {
        let lua = Arc::clone(&lua);
        let inner = Arc::clone(&inner);
        fakes::within(
            "lua.with_quota",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || lua.with_quota(inner, pair(), None),
        )
    };
    assert!(plain.is_err(), "the broken provider fails with_quota");
    let observed = {
        let lua = Arc::clone(&lua);
        let inner = Arc::clone(&inner);
        fakes::within(
            "lua.with_quota_observed",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            move || lua.with_quota_observed(inner, Some(observe), pair(), None),
        )
    };
    assert!(
        observed.is_err(),
        "the broken provider fails with_quota_observed the same way"
    );
}

#[test]
fn quota_pending_is_true_only_before_any_fetch_finishes_or_fails() {
    let root = fakes::TempDir::new("fiber-quota-pending");
    let (provider, clock) = quota_provider(&root, "nil");
    // A fresh label never fetched: pending.
    assert!(fakes::within(
        "quota_pending",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = provider.clone();
            move || provider.quota_pending(&pair())
        }
    ));
    // After a fetch succeeds: no longer pending.
    let now = clock.now();
    seed(
        &provider,
        Some((Some(percent("primary", 10.0, None)), now)),
        None,
    );
    assert!(!fakes::within(
        "quota_pending",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = provider.clone();
            move || provider.quota_pending(&pair())
        }
    ));
    // After a first fetch fails with no last value: not pending either.
    seed(&provider, None, None);
    lock(&provider.quotas.state)
        .get_mut(&pair())
        .unwrap()
        .failed = true;
    assert!(!fakes::within(
        "quota_pending",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = provider.clone();
            move || provider.quota_pending(&pair())
        }
    ));
    // While the VM is busy the call still returns at once: no wait, no Lua.
    let root = fakes::TempDir::new("fiber-quota-pending-busy");
    let server = fakes::ProviderServer::start([ok(windows_body("primary", 10.0, None))]).unwrap();
    server.hold();
    let busy = http_quota_provider(&root, &server.url(), FakeClock::new());
    assert!(fakes::within(
        "quota_pending",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let busy = busy.clone();
            move || busy.quota_pending(&pair())
        }
    ));
    server.release();
}

/// Waits for the flight `wait` names to end, failing at the suite's
/// deadline instead of parking past it.
#[track_caller]
fn wait_flight(provider: &Arc<LuaProvider>, wait: QuotaFlight) -> QuotaEntry {
    let provider = Arc::clone(provider);
    let pair = pair();
    fakes::within(
        "provider.wait_quota",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || provider.wait_quota(&pair, wait),
    )
}

#[test]
fn force_start_fetches_whatever_the_cache_age() {
    let root = fakes::TempDir::new("fiber-quota-force-start");
    let server = fakes::ProviderServer::start([
        ok(windows_body("primary", 10.0, None)),
        ok(windows_body("primary", 11.0, None)),
    ])
    .unwrap();
    let provider = http_quota_provider(&root, &server.url(), FakeClock::new());
    let first = QuotaEntry::Reported(percent("primary", 10.0, None));
    assert_eq!(read(&provider), first);
    assert_eq!(server.requests().len(), 1);
    // The cache is fresh: the fake clock never moved, so a due-check
    // would never refetch. Only the force-start starts a new fetch.
    let wait = provider.force_start_quota(&pair(), None);
    assert!(
        server.await_requests(2, Deadline::after(WAIT)),
        "the force-start never reached the server"
    );
    assert_eq!(
        wait_flight(&provider, wait),
        QuotaEntry::Reported(percent("primary", 11.0, None))
    );
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn force_start_shares_an_in_flight_flight() {
    let root = fakes::TempDir::new("fiber-quota-force-shared");
    let server = fakes::ProviderServer::start([ok(windows_body("primary", 10.0, None))]).unwrap();
    server.hold();
    let provider = http_quota_provider(&root, &server.url(), FakeClock::new());
    let first = provider.force_start_quota(&pair(), None);
    assert!(
        server.await_requests(1, Deadline::after(WAIT)),
        "the first force-start never reached the server"
    );
    // The flight is still held open: the second force-start shares it,
    // starting no fetch of its own.
    let second = provider.force_start_quota(&pair(), None);
    assert_eq!(
        server.requests().len(),
        1,
        "the shared force-start fetched again"
    );
    server.release();
    let want = QuotaEntry::Reported(percent("primary", 10.0, None));
    assert_eq!(wait_flight(&provider, first), want);
    assert_eq!(wait_flight(&provider, second), want);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_wait_reports_its_own_flight_when_the_flight_is_replaced() {
    let root = fakes::TempDir::new("fiber-quota-wait-replaced");
    let (provider, clock) = quota_provider(&root, "nil");
    let pair = pair();
    let t0 = clock.now();
    let value = percent("primary", 42.5, None);
    seed(
        &provider,
        Some((Some(value.clone()), t0)),
        Some(Flight { id: 7, started: t0 }),
    );
    // The force-start shares the seeded flight instead of starting one:
    // the wait below names flight 7.
    let wait = provider.force_start_quota(&pair, None);
    assert_eq!(wait.id, 7, "the force-start did not share flight 7");
    let (tx, rx) = mpsc::channel();
    let reader = {
        let provider = Arc::clone(&provider);
        let pair = pair.clone();
        std::thread::spawn(move || tx.send(provider.wait_quota(&pair, wait)))
    };
    assert!(
        clock.await_parked(t0 + Duration::from_secs(8), fakes::Deadline::after(WAIT)),
        "the wait never parked on flight 7"
    );
    // Hold the cache while the clock passes flight 7's deadline, so the
    // wait wakes only after flight 7 has expired and flight 8 is in the
    // slot: the wait must report on flight 7, not wait on flight 8.
    {
        let mut states = lock(&provider.quotas.state);
        clock.advance_when_parked(
            Some(t0 + Duration::from_secs(8)),
            Some(reader.thread().id()),
            Duration::from_secs(8),
            &Deadline::after(WAIT),
        );
        let now = clock.now();
        let cache = states.get_mut(&pair).unwrap();
        assert!(expire_overdue(cache, now, &provider.quotas.parker));
        let id = cache.next_id;
        cache.next_id = id.wrapping_add(1);
        cache.flight = Some(Flight { id, started: now });
    }
    match Deadline::after(WAIT).recv(&rx) {
        Ok(found) => assert_eq!(
            found,
            QuotaEntry::Unreachable {
                last: Some((value, Duration::from_secs(8))),
            },
            "the wait did not report on its own flight"
        ),
        Err(_) => panic!("the wait held past its own flight's deadline"),
    }
}
