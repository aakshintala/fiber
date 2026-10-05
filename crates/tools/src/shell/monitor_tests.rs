//! The splitter, the cuts, the budget and the flood clock, and the feed
//! that runs them.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use contract::JobId;
use contract::clock::Clock as _;
use contract::events::JobLine;
use contract::jobs::{Lines, Stop};
use fakes::clock::FakeClock;

use super::{Admit, Budget, Feed, Splitter, batch};
use crate::shell::output::{Shared, lock};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn the_splitter_keeps_an_incomplete_line_until_it_completes_or_is_flushed() {
    let mut splitter = Splitter::default();
    assert!(splitter.take(b"par").is_empty());
    assert_eq!(
        splitter.take(b"tial\nnext\n\nlast"),
        ["partial", "next", ""]
    );
    assert_eq!(splitter.take(b" bit"), Vec::<String>::new());
    assert_eq!(splitter.flush().as_deref(), Some("last bit"));
    assert_eq!(splitter.flush(), None);
}

#[test]
fn the_splitter_joins_a_character_split_across_reads_and_decodes_lossily() {
    let mut splitter = Splitter::default();
    let e = "é".as_bytes();
    assert!(splitter.take(&e[..1]).is_empty());
    let mut rest = e[1..].to_vec();
    rest.extend_from_slice(b"\n\xff\n");
    assert_eq!(splitter.take(&rest), ["é", "\u{fffd}"]);
}

#[test]
fn a_line_is_cut_after_500_characters() {
    let at = "é".repeat(500);
    assert_eq!(batch(std::slice::from_ref(&at)), at);
    let over = "é".repeat(501);
    assert_eq!(batch(&[over]), format!("{at} [cut]"));
    let under = "é".repeat(499);
    assert_eq!(batch(std::slice::from_ref(&under)), under);
}

#[test]
fn a_delivery_is_cut_after_3000_characters() {
    // Six lines of 499 characters and their five newlines: 2,999.
    let line = "ü".repeat(499);
    let under = vec![line.clone(); 6];
    let joined = under.join("\n");
    assert_eq!(joined.chars().count(), 2_999);
    assert_eq!(batch(&under), joined);
    let mut at = under.clone();
    at[5].push('ü');
    assert_eq!(batch(&at).chars().count(), 3_000);
    assert!(!batch(&at).contains("[cut"));
    let mut over = at.clone();
    over.push("ab".to_owned());
    let whole = over.join("\n");
    let kept: String = whole.chars().take(3_000).collect();
    assert_eq!(batch(&over), format!("{kept}\n[cut: 3 more characters]"));
}

#[test]
fn the_budget_spends_ten_then_drops() {
    let t0 = FakeClock::new().now();
    let mut budget = Budget::new(t0);
    for _ in 0..10 {
        assert_eq!(budget.admit(t0), Admit::Send(None));
    }
    assert_eq!(budget.admit(t0), Admit::Drop { flood: false });
}

#[test]
fn one_delivery_is_added_every_two_seconds() {
    let t0 = FakeClock::new().now();
    let mut budget = Budget::new(t0);
    for _ in 0..10 {
        budget.admit(t0);
    }
    assert_eq!(budget.admit(t0 + ms(1_999)), Admit::Drop { flood: false });
    assert_eq!(budget.admit(t0 + ms(2_000)), Admit::Send(Some(1)));
    assert_eq!(budget.admit(t0 + ms(2_000)), Admit::Drop { flood: false });
    // Two more intervals: two more, the first carrying the one dropped.
    assert_eq!(budget.admit(t0 + ms(6_000)), Admit::Send(Some(1)));
    assert_eq!(budget.admit(t0 + ms(6_000)), Admit::Send(None));
    assert_eq!(budget.admit(t0 + ms(6_000)), Admit::Drop { flood: false });
    // A part interval carries over: 3,000 ms after 6,000 adds one.
    assert_eq!(budget.admit(t0 + ms(9_000)), Admit::Send(Some(1)));
    assert_eq!(budget.admit(t0 + ms(9_999)), Admit::Drop { flood: false });
    assert_eq!(budget.admit(t0 + ms(10_000)), Admit::Send(Some(1)));
}

#[test]
fn a_long_idle_gap_fills_the_budget_to_ten_and_no_more() {
    let t0 = FakeClock::new().now();
    let mut budget = Budget::new(t0);
    for _ in 0..10 {
        budget.admit(t0);
    }
    let later = t0 + Duration::from_secs(365 * 24 * 3_600);
    for _ in 0..10 {
        assert!(matches!(budget.admit(later), Admit::Send(_)));
    }
    assert_eq!(budget.admit(later), Admit::Drop { flood: false });
}

#[test]
fn a_token_spent_from_a_full_budget_returns_a_whole_interval_later() {
    let t0 = FakeClock::new().now();
    let mut budget = Budget::new(t0);
    // Full and idle until 1,500: the refill clock restarts there.
    for _ in 0..10 {
        assert_eq!(budget.admit(t0 + ms(1_500)), Admit::Send(None));
    }
    assert_eq!(budget.admit(t0 + ms(3_499)), Admit::Drop { flood: false });
    assert_eq!(budget.admit(t0 + ms(3_500)), Admit::Send(Some(1)));
}

#[test]
fn the_suppressed_count_rides_on_the_next_delivery_and_resets() {
    let t0 = FakeClock::new().now();
    let mut budget = Budget::new(t0);
    for _ in 0..10 {
        budget.admit(t0);
    }
    for _ in 0..3 {
        budget.admit(t0);
    }
    assert_eq!(budget.admit(t0 + ms(2_000)), Admit::Send(Some(3)));
    assert_eq!(budget.admit(t0 + ms(4_000)), Admit::Send(None));
    budget.admit(t0 + ms(4_000));
    assert_eq!(budget.take_suppressed(), Some(1));
    assert_eq!(budget.take_suppressed(), None);
}

/// Offers deliveries at `now` until one is dropped; that drop's flood.
fn drop_at(budget: &mut Budget, now: Instant) -> bool {
    loop {
        if let Admit::Drop { flood } = budget.admit(now) {
            return flood;
        }
    }
}

#[test]
fn a_flood_is_a_drop_thirty_seconds_into_a_run_that_sends_do_not_end() {
    let t0 = FakeClock::new().now();
    let mut budget = Budget::new(t0);
    // The run starts at the first drop. A send every refill does not end it.
    assert!(!drop_at(&mut budget, t0));
    for k in 1..30 {
        assert!(!drop_at(&mut budget, t0 + ms(k * 1_000)), "{k}");
    }
    assert!(!drop_at(&mut budget, t0 + ms(29_999)));
    assert!(drop_at(&mut budget, t0 + ms(30_000)));
}

#[test]
fn two_seconds_without_a_drop_ends_the_run() {
    let t0 = FakeClock::new().now();
    let mut budget = Budget::new(t0);
    for k in 0..=10 {
        assert!(!drop_at(&mut budget, t0 + ms(k * 1_000)));
    }
    // 2,000 ms after the last drop: the run ends, and a new one starts.
    let restart = t0 + ms(12_000);
    for k in 0..30 {
        assert!(!drop_at(&mut budget, restart + ms(k * 1_000)), "{k}");
    }
    assert!(!drop_at(&mut budget, restart + ms(29_999)));
    assert!(drop_at(&mut budget, restart + ms(30_000)));
}

#[test]
fn a_gap_just_under_two_seconds_keeps_the_run() {
    let t0 = FakeClock::new().now();
    let mut budget = Budget::new(t0);
    for k in 0..=10 {
        assert!(!drop_at(&mut budget, t0 + ms(k * 1_000)));
    }
    let mut at = t0 + ms(11_999);
    while at < t0 + ms(30_000) {
        assert!(!drop_at(&mut budget, at));
        at += ms(1_000);
    }
    assert!(drop_at(&mut budget, t0 + ms(30_000)));
}

struct Fed {
    clock: Arc<FakeClock>,
    shared: Shared,
    sent: Arc<Mutex<Vec<JobLine>>>,
    stops: Arc<Mutex<u32>>,
    feed: Feed,
}

fn fed() -> Fed {
    let clock = FakeClock::new();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let stops = Arc::new(Mutex::new(0));
    let (record, count) = (Arc::clone(&sent), Arc::clone(&stops));
    let feed = Feed::new(
        JobId("j_1".into()),
        Lines(Box::new(move |line| record.lock().unwrap().push(line))),
        Stop(Box::new(move || *count.lock().unwrap() += 1)),
        clock.now(),
    );
    let shared = Shared::default();
    lock(&shared.inner).lines = Some(Vec::new());
    Fed {
        clock,
        shared,
        sent,
        stops,
        feed,
    }
}

fn line(lines: &str, suppressed: Option<u64>) -> JobLine {
    JobLine {
        job_id: JobId("j_1".into()),
        lines: lines.into(),
        suppressed,
    }
}

impl Fed {
    fn print(&self, bytes: &[u8]) {
        lock(&self.shared.inner)
            .lines
            .as_mut()
            .unwrap()
            .extend_from_slice(bytes);
    }

    fn sent(&self) -> Vec<JobLine> {
        self.sent.lock().unwrap().clone()
    }
}

#[test]
fn a_pass_sends_every_complete_line_as_one_batch() {
    let mut fed = fed();
    fed.feed.pass(&fed.shared, fed.clock.as_ref(), true);
    assert!(fed.sent().is_empty(), "no lines, no batch");
    fed.print(b"a\nb\nc");
    fed.feed.pass(&fed.shared, fed.clock.as_ref(), true);
    fed.print(b"d\n");
    fed.feed.pass(&fed.shared, fed.clock.as_ref(), true);
    assert_eq!(fed.sent(), [line("a\nb", None), line("cd", None)]);
}

#[test]
fn the_end_flushes_the_last_line_then_the_suppressed_count() {
    let mut fed = fed();
    for _ in 0..12 {
        fed.print(b"x\n");
        fed.feed.pass(&fed.shared, fed.clock.as_ref(), true);
    }
    fed.print(b"tail");
    fed.feed.finish(&fed.shared, fed.clock.as_ref());
    let sent = fed.sent();
    assert_eq!(sent.len(), 11);
    // The flushed line found the budget empty too: three dropped in all.
    assert_eq!(sent[10], line("", Some(3)));
    assert!(!fed.feed.flooded());
}

#[test]
fn the_end_sends_the_last_line_when_the_budget_allows_and_no_count_when_none_was_dropped() {
    let mut fed = fed();
    fed.print(b"one\ntail");
    fed.feed.finish(&fed.shared, fed.clock.as_ref());
    assert_eq!(fed.sent(), [line("one", None), line("tail", None)]);
}

fn flood(fed: &mut Fed, running: bool) {
    // Drops from the start, one pass every second, for 30 seconds.
    for _ in 0..=30 {
        for _ in 0..12 {
            fed.print(b"x\n");
            fed.feed.pass(&fed.shared, fed.clock.as_ref(), running);
        }
        fed.clock.advance(ms(1_000));
    }
}

#[test]
fn a_flood_while_running_stops_the_monitor_once() {
    let mut fed = fed();
    flood(&mut fed, true);
    assert!(fed.feed.flooded());
    assert_eq!(*fed.stops.lock().unwrap(), 1);
}

#[test]
fn a_flood_found_after_a_stop_began_is_not_marked() {
    let mut fed = fed();
    flood(&mut fed, false);
    assert!(!fed.feed.flooded());
    assert_eq!(*fed.stops.lock().unwrap(), 0);
}
