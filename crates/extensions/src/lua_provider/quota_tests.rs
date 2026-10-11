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
fn quota_provider_on(
    root: &fakes::TempDir,
    run: &str,
    clock: Arc<FakeClock>,
) -> Arc<LuaProvider> {
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

/// A provider `p` whose `quota()` returns `run`, and its clock.
fn quota_provider(root: &fakes::TempDir, run: &str) -> (Arc<LuaProvider>, Arc<FakeClock>) {
    let clock = FakeClock::new();
    let provider = quota_provider_on(root, run, Arc::clone(&clock));
    (provider, clock)
}

fn pair() -> CredentialPair {
    CredentialPair {
        credential: "codex".to_owned(),
        label: "work".to_owned(),
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
            resets_at: resets_at
                .map(|secs| UNIX_EPOCH + Duration::from_secs(secs)),
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
        (
            "{ credit = { remaining = \"lots\" } }",
            unreachable.clone(),
        ),
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
