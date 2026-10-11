use super::*;
use contract::events::CacheLifetime;
use contract::provider::ModelRequest;
use fakes::clock::FakeClock;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// One origin; every instant below is built from it, so no clock is read.
fn origin() -> Instant {
    FakeClock::new().origin()
}

fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: String::new(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".to_owned(),
        cache_lifetime: CacheLifetime::FiveMinutes,
        cache_key: String::new(),
        conversation: Vec::new(),
        previous_end: None,
        sent_tools: None,
        max_output_tokens: None,
        session_dir: PathBuf::new(),
    }
}

fn hour() -> Duration {
    Duration::from_secs(3600)
}

fn five_minutes() -> Duration {
    Duration::from_secs(300)
}

#[test]
fn a_cap_warms_once_a_request_is_recorded() {
    let t0 = origin();
    let mut warming = Warming::default();
    warming.set_cap(Some(2));
    warming.record(&request(), t0);
    assert_eq!(warming.stop_at(t0, hour()), Some(t0 + 2 * hour()));
}

#[test]
fn changing_the_cap_keeps_the_held_request() {
    let t0 = origin();
    let mut warming = Warming::default();
    warming.set_cap(Some(1));
    warming.record(&request(), t0);
    warming.set_cap(Some(2));
    assert_eq!(warming.stop_at(t0, hour()), Some(t0 + 2 * hour()));
    assert!(
        warming
            .resend(t0 + Duration::from_secs(10), t0 + 2 * hour(), hour())
            .is_some()
    );
}

#[test]
fn no_request_recorded_means_no_stop() {
    let t0 = origin();
    let mut warming = Warming::default();
    warming.set_cap(Some(1));
    assert_eq!(warming.stop_at(t0, hour()), None);
}

#[test]
fn a_cap_of_zero_stops_at_the_start() {
    let t0 = origin();
    let mut warming = Warming::default();
    warming.set_cap(Some(0));
    warming.record(&request(), t0);
    assert_eq!(warming.stop_at(t0, hour()), Some(t0));
}

#[test]
fn warming_off_holds_no_request() {
    let t0 = origin();
    let t1 = t0 + Duration::from_secs(1);
    let mut warming = Warming::default();
    warming.record(&request(), t0);
    warming.stop(t1);
    assert_eq!(warming.take_stopped(), None);
}

#[test]
fn a_switch_with_a_request_held_stops_warming_once() {
    let t0 = origin();
    let t1 = t0 + Duration::from_secs(1);
    let mut warming = Warming::default();
    warming.set_cap(Some(1));
    warming.record(&request(), t0);
    warming.stop(t1);
    assert_eq!(warming.take_stopped(), Some(t1));
    assert_eq!(warming.take_stopped(), None);
    assert_eq!(warming.stop_at(t0, hour()), None);
}

#[test]
fn a_switch_with_nothing_held_stamps_nothing() {
    let t0 = origin();
    let t1 = t0 + Duration::from_secs(1);
    let mut warming = Warming::default();
    warming.set_cap(Some(1));
    warming.stop(t1);
    assert_eq!(warming.take_stopped(), None);
}

#[test]
fn a_refresh_comes_due_the_margin_before_the_lifetime_ends() {
    let t0 = origin();
    let mut warming = Warming::default();
    warming.set_cap(Some(1));
    warming.record(&request(), t0);
    assert_eq!(
        warming.due(t0 + hour(), five_minutes()),
        Some(t0 + Duration::from_secs(270))
    );
}

#[test]
fn a_refresh_due_at_the_stop_is_none() {
    let t0 = origin();
    let mut warming = Warming::default();
    warming.set_cap(Some(1));
    warming.record(&request(), t0);
    assert_eq!(
        warming.due(t0 + Duration::from_secs(270), five_minutes()),
        None
    );
}

#[test]
fn a_send_moves_the_instant_due_counts_from() {
    let t0 = origin();
    let mut warming = Warming::default();
    warming.set_cap(Some(1));
    warming.record(&request(), t0);
    warming.sent(t0 + Duration::from_secs(270));
    assert_eq!(
        warming.due(t0 + hour(), five_minutes()),
        Some(t0 + Duration::from_secs(540))
    );
}

#[test]
fn resend_is_the_request_capped_at_one_output_token() {
    let t0 = origin();
    let mut warming = Warming::default();
    warming.set_cap(Some(1));
    warming.record(&request(), t0);
    let mut expected = request();
    expected.max_output_tokens = Some(1);
    assert_eq!(
        warming.resend(t0 + Duration::from_secs(10), t0 + hour(), five_minutes()),
        Some(expected)
    );
}

#[test]
fn resend_is_none_once_the_cache_expired() {
    let t0 = origin();
    let mut warming = Warming::default();
    warming.set_cap(Some(1));
    warming.record(&request(), t0);
    assert_eq!(
        warming.resend(t0 + five_minutes(), t0 + hour(), five_minutes()),
        None
    );
}

#[test]
fn resend_is_none_at_the_stop() {
    let t0 = origin();
    let mut warming = Warming::default();
    warming.set_cap(Some(1));
    warming.record(&request(), t0);
    let at = t0 + Duration::from_secs(100);
    assert_eq!(warming.resend(at, at, five_minutes()), None);
}

#[test]
fn resend_with_nothing_held_is_none() {
    let t0 = origin();
    let warming = {
        let mut warming = Warming::default();
        warming.set_cap(Some(1));
        warming
    };
    assert_eq!(
        warming.resend(t0 + Duration::from_secs(10), t0 + hour(), five_minutes()),
        None
    );
}

#[test]
fn held_is_none_without_a_cap_and_some_once_recorded() {
    let t0 = origin();
    let mut warming = Warming::default();
    assert!(warming.held().is_none());
    warming.set_cap(Some(1));
    assert!(warming.held().is_none());
    warming.record(&request(), t0);
    assert!(warming.held().is_some());
}
