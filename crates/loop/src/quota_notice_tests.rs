use super::{Due, Fired, text};
use contract::events::{Event, QuotaCredit, QuotaInfo, QuotaNoticed, QuotaRearmed};
use serde_json::{Value, json};

fn credit(remaining: f64, limit: Option<f64>) -> QuotaInfo {
    QuotaInfo {
        provider: "openrouter".into(),
        credential: "work".into(),
        windows: None,
        credit: Some(QuotaCredit { remaining, limit }),
        unreachable: None,
    }
}

fn credit_notice(percent_used: f64) -> Event {
    Event::QuotaNoticed(QuotaNoticed {
        provider: "openrouter".into(),
        credential: "work".into(),
        window: "credit".into(),
        percent_used,
        resets_at: None,
        notice_at: 80.0,
    })
}

fn credit_rearm(percent_used: f64) -> Event {
    Event::QuotaRearmed(QuotaRearmed {
        provider: "openrouter".into(),
        credential: "work".into(),
        window: "credit".into(),
        percent_used,
        notice_at: 80.0,
    })
}

fn fired_credit() -> Fired {
    let mut state = Fired::default();
    state.fold(&credit_notice(90.0));
    state
}

#[test]
fn credit_percent_is_the_used_share_of_the_limit() {
    for (remaining, limit, used) in [
        (2.0, 10.0, Some(80.0)),
        (1.0, 10.0, Some(90.0)),
        (0.5, 4.0, Some(87.5)),
        (3.0, 4.0, None),
        (2.01, 10.0, None),
        (1.99, 10.0, Some(80.1)),
    ] {
        expect(
            Fired::default().judge(&[credit(remaining, Some(limit))], 80.0, 100),
            used.map(credit_notice).into_iter().collect(),
        );
    }
}

#[test]
fn credit_percent_clamps_to_0_and_100() {
    expect(
        Fired::default().judge(&[credit(-5.0, Some(10.0))], 80.0, 100),
        vec![credit_notice(100.0)],
    );
    expect(
        fired_credit().judge(&[credit(15.0, Some(10.0))], 80.0, 100),
        vec![credit_rearm(0.0)],
    );
}

#[test]
fn a_fired_credit_read_hot_stays_silent() {
    for now in [0, 100, u64::MAX] {
        expect(
            fired_credit().judge(&[credit(1.0, Some(10.0))], 80.0, now),
            vec![],
        );
    }
}

#[test]
fn a_fired_credit_rearms_only_on_a_reading_below_notice_at() {
    let mut state = fired_credit();
    expect(
        state.judge(&[credit(3.0, Some(4.0))], 80.0, 100),
        vec![credit_rearm(25.0)],
    );
    state.fold(&credit_rearm(25.0));
    expect(state.judge(&[credit(3.0, Some(4.0))], 80.0, 100), vec![]);
    expect(
        state.judge(&[credit(1.0, Some(10.0))], 80.0, 100),
        vec![credit_notice(90.0)],
    );
}

#[test]
fn a_credit_without_a_positive_limit_never_notices() {
    for state in [Fired::default(), fired_credit()] {
        for limit in [None, Some(0.0), Some(-10.0), Some(f64::NAN)] {
            expect(state.judge(&[credit(0.0, limit)], 80.0, 100), vec![]);
        }
        expect(
            state.judge(&[credit(f64::NAN, Some(10.0))], 80.0, 100),
            vec![],
        );
        expect(state.judge(&[credit(1.0, Some(0.0))], 80.0, 100), vec![]);
    }
}

fn row(windows: Value) -> QuotaInfo {
    serde_json::from_value(json!({"provider":"codex","credential":"work","windows":windows}))
        .unwrap()
}

fn window(percent: f64, resets_at: Option<u64>) -> QuotaInfo {
    row(json!([{"name":"weekly","percent_used":percent,"resets_at":resets_at}]))
}

fn noticed(percent: f64, resets_at: Option<u64>) -> Event {
    Event::QuotaNoticed(QuotaNoticed {
        provider: "codex".into(),
        credential: "work".into(),
        window: "weekly".into(),
        percent_used: percent,
        resets_at,
        notice_at: 80.0,
    })
}

fn rearmed(percent: f64) -> Event {
    Event::QuotaRearmed(QuotaRearmed {
        provider: "codex".into(),
        credential: "work".into(),
        window: "weekly".into(),
        percent_used: percent,
        notice_at: 80.0,
    })
}

fn fired(resets_at: Option<u64>) -> Fired {
    let mut fired = Fired::default();
    fired.fold(&noticed(90.0, resets_at));
    fired
}

fn expect(due: Due, events: Vec<Event>) {
    let lines: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Event::QuotaNoticed(notice) = event {
                Some(text(notice))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(due.events, events);
    assert_eq!(due.lines, lines);
}

#[test]
fn quota_notice_threshold_and_rearm_boundaries() {
    for percent in [79.9, 80.0, 80.1] {
        expect(
            Fired::default().judge(&[window(percent, None)], 80.0, 100),
            if percent < 80.0 {
                vec![]
            } else {
                vec![noticed(percent, None)]
            },
        );
        expect(
            fired(None).judge(&[window(percent, None)], 80.0, 100),
            if percent < 80.0 {
                vec![rearmed(percent)]
            } else {
                vec![]
            },
        );
    }
    expect(fired(None).judge(&[window(95.0, None)], 80.0, 100), vec![]);
}

#[test]
fn quota_notice_recorded_and_reading_reset_edges() {
    for reset in [99, 100, 101] {
        expect(
            fired(Some(reset)).judge(&[window(90.0, Some(200))], 80.0, 100),
            if reset <= 100 {
                vec![noticed(90.0, Some(200))]
            } else {
                vec![]
            },
        );
        expect(
            fired(Some(reset)).judge(&[window(10.0, None)], 80.0, 100),
            if reset <= 100 {
                vec![]
            } else {
                vec![rearmed(10.0)]
            },
        );
        expect(
            Fired::default().judge(&[window(90.0, Some(reset))], 80.0, 100),
            if reset <= 100 {
                vec![]
            } else {
                vec![noticed(90.0, Some(reset))]
            },
        );
    }
}

#[test]
fn quota_notice_reset_and_unreachable_keep_fired_state() {
    for state in [Fired::default(), fired(None)] {
        expect(
            state.judge(&[row(json!([{"name":"weekly","reset":true}]))], 80.0, 100),
            vec![],
        );
        for percent in [10.0, 90.0] {
            let mut reading = window(percent, None);
            reading.unreachable = Some(contract::shapes::True);
            reading.credit = Some(contract::events::QuotaCredit {
                remaining: 1.0,
                limit: Some(10.0),
            });
            expect(state.judge(&[reading], 80.0, 100), vec![]);
        }
        expect(
            state.judge(&[row(json!([{"name":"weekly"}]))], 80.0, 100),
            vec![],
        );
    }
}

#[test]
fn quota_notice_reader_order_and_independent_keys() {
    let reading =
        row(json!([{"name":"weekly","percent_used":90.0},{"name":"5h","percent_used":85.0}]));
    let mut second = noticed(85.0, None);
    if let Event::QuotaNoticed(n) = &mut second {
        n.window = "5h".into();
    }
    expect(
        Fired::default().judge(&[reading], 80.0, 100),
        vec![noticed(90.0, None), second],
    );
    for field in ["credential", "provider"] {
        let first = window(90.0, None);
        let mut other = first.clone();
        let mut event = noticed(90.0, None);
        if field == "credential" {
            other.credential = "home".into();
            if let Event::QuotaNoticed(n) = &mut event {
                n.credential = "home".into();
            }
        } else {
            other.provider = "claude".into();
            if let Event::QuotaNoticed(n) = &mut event {
                n.provider = "claude".into();
            }
        }
        expect(
            Fired::default().judge(&[first, other], 80.0, 100),
            vec![noticed(90.0, None), event],
        );
    }
}

#[test]
fn quota_notice_duplicates_keep_the_first_reading() {
    for first in [10.0, 90.0] {
        let reading = row(
            json!([{"name":"weekly","percent_used":first},{"name":"weekly","percent_used":95.0}]),
        );
        expect(
            Fired::default().judge(std::slice::from_ref(&reading), 80.0, 100),
            if first < 80.0 {
                vec![]
            } else {
                vec![noticed(first, None)]
            },
        );
        expect(
            fired(None).judge(&[reading], 80.0, 100),
            if first < 80.0 {
                vec![rearmed(first)]
            } else {
                vec![]
            },
        );
    }
}

#[test]
fn quota_notice_uses_the_readers_threshold() {
    let mut event = noticed(60.0, None);
    if let Event::QuotaNoticed(n) = &mut event {
        n.notice_at = 50.0;
    }
    expect(
        Fired::default().judge(&[window(60.0, None)], 50.0, 100),
        vec![event],
    );
}

#[test]
fn quota_notice_fold_adds_removes_and_ignores_other_keys_and_events() {
    let mut state = fired(None);
    let mut other = rearmed(10.0);
    if let Event::QuotaRearmed(n) = &mut other {
        n.credential = "other".into();
    }
    state.fold(&other);
    state.fold(&Event::QuotaNoticed(QuotaNoticed {
        provider: "other".into(),
        credential: "work".into(),
        window: "weekly".into(),
        percent_used: 90.0,
        resets_at: None,
        notice_at: 80.0,
    }));
    state.fold(&Event::QuotaRearmed(QuotaRearmed {
        provider: "codex".into(),
        credential: "work".into(),
        window: "other".into(),
        percent_used: 10.0,
        notice_at: 80.0,
    }));
    state.fold(&Event::Notice(contract::events::Notice {
        code: contract::ErrorCode::NoModel,
        message: "unrelated".into(),
        extension: None,
    }));
    expect(state.judge(&[window(90.0, None)], 80.0, 100), vec![]);
    state.fold(&rearmed(10.0));
    expect(
        state.judge(&[window(90.0, None)], 80.0, 100),
        vec![noticed(90.0, None)],
    );
}

#[test]
fn quota_notice_text_rounds_and_omits_absent_reset_clause() {
    for (percent, reset, expected) in [
        (
            83.5,
            Some(1760000000000),
            "Fiber: codex credential work has used 84% of its weekly quota, which resets at 2025-10-09T08:53:20Z.",
        ),
        (
            83.5,
            None,
            "Fiber: codex credential work has used 84% of its weekly quota.",
        ),
        (
            80.0,
            None,
            "Fiber: codex credential work has used 80% of its weekly quota.",
        ),
    ] {
        let Event::QuotaNoticed(notice) = noticed(percent, reset) else {
            unreachable!()
        };
        assert_eq!(text(&notice), expected);
    }
}

#[test]
fn quota_notice_credit_is_a_window_only_with_a_positive_limit() {
    for limit in [None, Some(0.0), Some(10.0)] {
        let reading: QuotaInfo = serde_json::from_value(json!({"provider":"openrouter","credential":"work","credit":{"remaining":1.0,"limit":limit}})).unwrap();
        let event = Event::QuotaNoticed(QuotaNoticed {
            provider: "openrouter".into(),
            credential: "work".into(),
            window: "credit".into(),
            percent_used: 90.0,
            resets_at: None,
            notice_at: 80.0,
        });
        if let Event::QuotaNoticed(n) = &event {
            assert_eq!(
                text(n),
                "Fiber: openrouter credential work has used 90% of its credit quota."
            );
        }
        expect(
            Fired::default().judge(&[reading], 80.0, 100),
            if limit == Some(10.0) {
                vec![event]
            } else {
                vec![]
            },
        );
    }
}
