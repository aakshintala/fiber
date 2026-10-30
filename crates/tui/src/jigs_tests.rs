//! Tests for the `open` jig: replaying an events log through the
//! terminal's own fold and draw, split into stages (`measure_open`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use contract::Envelope;
use serde_json::json;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// One session line of `kind`, serialized as the jig reads it.
fn event(kind: &str) -> String {
    let envelope = Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::Map::new(),
    };
    serde_json::to_string(&envelope).unwrap_or_default()
}

/// `n` session lines, one envelope per line.
fn events(n: usize) -> String {
    (0..n)
        .map(|_| event("turn_started"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A clock that ticks one millisecond per `now()`, so the stages hold
/// nonzero durations. The origin is the fake clock's, since reading the
/// process clock is banned in tests.
struct TickClock {
    origin: std::time::Instant,
    ticks: AtomicU64,
}

impl TickClock {
    fn clock(origin: std::time::Instant) -> Arc<Self> {
        Arc::new(Self {
            origin,
            ticks: AtomicU64::new(0),
        })
    }
}

impl contract::clock::Clock for TickClock {
    fn now(&self) -> std::time::Instant {
        let ticks = self.ticks.fetch_add(1, Ordering::SeqCst);
        self.origin
            .checked_add(Duration::from_millis(ticks))
            .unwrap_or(self.origin)
    }

    fn wall(&self) -> std::time::SystemTime {
        std::time::SystemTime::UNIX_EPOCH
    }

    fn sleep(&self, _d: Duration) {}

    fn wait_until(
        &self,
        _until: Option<std::time::Instant>,
        wait: &mut dyn FnMut(Option<Duration>),
    ) {
        wait(Some(Duration::ZERO));
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

#[test]
fn measure_open_draws_one_frame_for_usize_max() {
    let clock = fakes::clock::FakeClock::new();
    let stages = crate::measure_open(&events(5), 60, 12, usize::MAX, clock)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(stages.frames, 1);
    assert!(stages.parse.is_zero());
    assert!(stages.fold.is_zero());
    assert!(stages.frame_time.is_zero());
    // One line is one frame with the same setting.
    let clock = fakes::clock::FakeClock::new();
    let one = crate::measure_open(&events(1), 60, 12, usize::MAX, clock)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(one.frames, 1);
    assert_ne!(one.frames, 2);
}

#[test]
fn measure_open_draws_after_every_k_lines_and_once_at_the_end() {
    // K = 3: K-1 lines draw only the final frame, K lines draw the one
    // periodic frame as the final one, K+1 lines draw one periodic frame
    // and the final one.
    for (lines, frames) in [(2, 1), (3, 1), (4, 2)] {
        let clock = fakes::clock::FakeClock::new();
        let stages = crate::measure_open(&events(lines), 60, 12, 3, clock)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(stages.frames, frames, "for {lines} lines at every 3");
    }
    // Every line draws at 1, and an empty log still draws its final frame.
    let clock = fakes::clock::FakeClock::new();
    let every =
        crate::measure_open(&events(4), 60, 12, 1, clock).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(every.frames, 4);
    assert_ne!(every.frames, 5);
    let clock = fakes::clock::FakeClock::new();
    let empty = crate::measure_open("", 60, 12, 3, clock).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(empty.frames, 1);
}

#[test]
fn measure_open_batch_boundaries_neither_add_nor_drop_frames() {
    // The jig folds in batches of up to HUB_BATCH lines, as the loop
    // does; crossing a batch boundary draws no frame of its own. 4095,
    // 4096 and 4097 straddle the first boundary.
    for (lines, frames) in [(4095, 1), (4096, 1), (4097, 1)] {
        let clock = fakes::clock::FakeClock::new();
        let stages = crate::measure_open(&events(lines), 60, 12, usize::MAX, clock)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(stages.frames, frames, "for {lines} lines ending at once");
    }
    for (lines, frames) in [(4095, 1), (4096, 1), (4097, 2)] {
        let clock = fakes::clock::FakeClock::new();
        let stages = crate::measure_open(&events(lines), 60, 12, 4096, clock)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(stages.frames, frames, "for {lines} lines every 4096");
    }
    // 4095 lines hold 63 full sixties and a partial frame, 4096 hold
    // 64 full ones, 4097 add the final frame.
    for (lines, frames) in [(4095, 64), (4096, 64), (4097, 65)] {
        let clock = fakes::clock::FakeClock::new();
        let stages = crate::measure_open(&events(lines), 60, 12, 64, clock)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(stages.frames, frames, "for {lines} lines every 64");
    }
}

#[test]
fn measure_open_batches_end_on_frame_boundaries_past_hub_batch() {
    // Fixed-size batches cross the frame boundary when `frame_every`
    // exceeds HUB_BATCH without dividing it: 10,000 lines at every
    // 5,000 land two frames on their boundaries, with the last periodic
    // frame covering the end. A `frame_every` of 4,097 just past the
    // loop's batch holds two full frames and the final one; 8,192 lines
    // at every 8,191 hold the one periodic frame and the final one.
    // Fixed-size batches of 4,096 draw off-boundary instead: 2, 2 and 1
    // frames for the three cases below.
    for (lines, frame_every, frames) in [(10_000, 5_000, 2), (10_000, 4_097, 3), (8_192, 8_191, 2)]
    {
        let clock = fakes::clock::FakeClock::new();
        let stages = crate::measure_open(&events(lines), 60, 12, frame_every, clock)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            stages.frames, frames,
            "for {lines} lines every {frame_every}"
        );
    }
}

#[test]
fn measure_open_skips_blank_lines_without_counting_them() {
    let clock = fakes::clock::FakeClock::new();
    let stages = crate::measure_open(
        &format!(
            "{}\n\n   \n{}",
            event("turn_started"),
            event("turn_started")
        ),
        60,
        12,
        usize::MAX,
        clock,
    )
    .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(stages.frames, 1);
}

#[test]
fn measure_open_names_the_unreadable_line() {
    let clock = fakes::clock::FakeClock::new();
    let error = crate::measure_open(
        &format!("{}\nnot json", events(1)),
        60,
        12,
        usize::MAX,
        clock,
    );
    assert!(
        matches!(&error, Err(error) if error.starts_with("line 2:")),
        "{error:?}"
    );
    // The first line failing names line 1, not line 0.
    let clock = fakes::clock::FakeClock::new();
    let error = crate::measure_open("not json", 60, 12, usize::MAX, clock);
    assert!(
        matches!(&error, Err(error) if error.starts_with("line 1:")),
        "{error:?}"
    );
}

#[test]
fn measure_open_times_each_stage_on_a_ticking_clock() {
    let origin = fakes::clock::FakeClock::new().origin();
    // Two lines at every 1: two periodic frames, no extra final one. Each
    // stage reads the clock twice per step, one tick apart, so each holds
    // one millisecond per step.
    let stages = crate::measure_open(&events(2), 60, 12, 1, TickClock::clock(origin))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(stages.frames, 2);
    assert_eq!(stages.parse, Duration::from_millis(2));
    assert_eq!(stages.fold, Duration::from_millis(2));
    assert_eq!(stages.frame_time, Duration::from_millis(2));
    // A frozen clock holds zeros, not the tick counts above.
    let clock = fakes::clock::FakeClock::new();
    let frozen =
        crate::measure_open(&events(2), 60, 12, 1, clock).unwrap_or_else(|error| panic!("{error}"));
    assert_ne!(frozen.parse, Duration::from_millis(2));
    assert!(frozen.frame_time.is_zero());
}

#[test]
fn measure_open_folds_a_frame_interval_in_one_batch() {
    let origin = fakes::clock::FakeClock::new().origin();
    // Five lines with no periodic frame fold in one batch: the fold
    // holds one batch's two clock reads, one millisecond. Folding one
    // line at a time would hold one millisecond per line, five.
    let stages = crate::measure_open(&events(5), 60, 12, usize::MAX, TickClock::clock(origin))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(stages.frames, 1);
    assert_eq!(stages.parse, Duration::from_millis(5));
    assert_eq!(stages.fold, Duration::from_millis(1));
}

#[test]
fn measure_open_reports_no_stages_for_an_empty_log_but_still_draws() {
    let origin = fakes::clock::FakeClock::new().origin();
    let stages = crate::measure_open("", 60, 12, usize::MAX, TickClock::clock(origin))
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(stages.frames, 1);
    assert!(stages.parse.is_zero());
    assert!(stages.fold.is_zero());
    assert_eq!(stages.frame_time, Duration::from_millis(1));
    assert_eq!(json!({"frames": stages.frames}), json!({"frames": 1}));
}

/// A session line with `seq`, serialized as the jig reads it.
fn numbered(seq: u64, kind: &str) -> String {
    let envelope = Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(contract::Seq(seq)),
        payload: serde_json::Map::new(),
    };
    serde_json::to_string(&envelope).unwrap_or_default()
}

/// A `Paging` over owned `lines`, with no hub connection: fetches never
/// go out, so any dropped page keeps the search scanning. The lines
/// outlive the paging within the caller's scope.
fn over<'a>(
    lines: &'a [(contract::Seq, String)],
    clock: std::sync::Arc<dyn contract::clock::Clock>,
) -> super::Paging<'a> {
    super::Paging {
        app: crate::app::App::new(None),
        log: lines
            .iter()
            .map(|(seq, line)| (*seq, line.as_str()))
            .collect(),
        area: ratatui::layout::Rect::new(0, 0, 60, 12),
        clock,
        most: 0,
    }
}

#[test]
fn range_lines_covers_the_inclusive_range() {
    let lines: Vec<(contract::Seq, String)> = [3u64, 4, 5]
        .into_iter()
        .map(|seq| (contract::Seq(seq), numbered(seq, "turn_started")))
        .collect();
    let clock = fakes::clock::FakeClock::new();
    let over = over(&lines, clock);
    // Both ends included: `<=` on either side undercounts.
    assert_eq!(over.range_lines(3, 5).unwrap_or_default().len(), 3);
    assert_eq!(over.range_lines(4, 4).unwrap_or_default().len(), 1);
    assert_eq!(over.range_lines(0, 2).unwrap_or_default().len(), 0);
}

#[test]
fn range_lines_tolerates_sparse_seqs_and_missing_ranges() {
    let lines: Vec<(contract::Seq, String)> = [1u64, 5, 10]
        .into_iter()
        .map(|seq| (contract::Seq(seq), numbered(seq, "turn_started")))
        .collect();
    let clock = fakes::clock::FakeClock::new();
    let sparse = over(&lines, clock);
    // Sparse seqs: entries below `from` are skipped, past `to` end it.
    assert_eq!(sparse.range_lines(0, 10).unwrap_or_default().len(), 3);
    assert_eq!(sparse.range_lines(2, 9).unwrap_or_default().len(), 1);
    // A range past every line is empty, not an error.
    assert!(sparse.range_lines(11, 20).unwrap_or_default().is_empty());
    assert!(sparse.range_lines(6, 6).unwrap_or_default().is_empty());
    // No lines at all is an empty range too.
    let empty: Vec<(contract::Seq, String)> = Vec::new();
    let clock = fakes::clock::FakeClock::new();
    let over_empty = over(&empty, clock);
    assert!(over_empty.range_lines(0, 10).unwrap_or_default().is_empty());
}

#[test]
fn range_lines_names_an_unreadable_line_in_range() {
    let lines = vec![
        (contract::Seq(1), numbered(1, "turn_started")),
        (contract::Seq(2), "not json".to_owned()),
    ];
    let clock = fakes::clock::FakeClock::new();
    let over = over(&lines, clock);
    let error = over.range_lines(1, 2).unwrap_err();
    assert!(!error.is_empty(), "an unreadable line errors");
    // Outside the range the bad line never reads.
    assert_eq!(over.range_lines(1, 1).unwrap_or_default().len(), 1);
}

/// An app folded with `lines` but never connected: `history` never goes
/// out, so any dropped page keeps the search scanning.
fn silent_app(lines: &[(contract::Seq, String)]) -> crate::app::App {
    let mut app = crate::app::App::new(None);
    app.set_size(60, 12);
    for (_, line) in lines {
        let envelope: contract::Envelope =
            serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}"));
        if app.session().is_none() {
            app.attach(envelope.session_id.clone());
        }
        let outgoing = app.on_line(crate::link::Line::Session(envelope));
        assert!(outgoing.is_empty(), "nothing sends while silent");
    }
    app
}

#[test]
fn an_absent_search_with_no_connection_never_completes() {
    // Enough bare turns to drop the first pages at 60x12.
    let lines: Vec<(contract::Seq, String)> = (1u64..=200)
        .map(|seq| (contract::Seq(seq), numbered(seq, "turn_started")))
        .collect();
    let app = silent_app(&lines);
    assert!(app.pages().part(0).is_none(), "the first page drops");
    let clock = fakes::clock::FakeClock::new();
    let log: Vec<(contract::Seq, &str)> = lines
        .iter()
        .map(|(seq, line)| (*seq, line.as_str()))
        .collect();
    let mut paging = super::Paging {
        app,
        log,
        area: ratatui::layout::Rect::new(0, 0, 60, 12),
        clock,
        most: 0,
    };
    // Still scanning with no outgoing: the dropped pages wait on a
    // fetch that never goes out, so the sample errors instead.
    let error = paging.search_absent().unwrap_err();
    assert!(
        error.contains("never completed"),
        "a stuck scan errors: {error}"
    );
}

/// A session line with `seq` and `payload`, serialized as the jig
/// reads it.
fn lined(seq: u64, kind: &str, action: Option<&str>, payload: serde_json::Value) -> String {
    let mut envelope: contract::Envelope =
        serde_json::from_str(&numbered(seq, kind)).unwrap_or_else(|error| panic!("{error}"));
    envelope.action_id = action.map(|id| contract::ActionId(id.to_owned()));
    envelope.payload = payload.as_object().cloned().unwrap_or_default();
    serde_json::to_string(&envelope).unwrap_or_default()
}

#[test]
fn an_accidental_absent_match_is_an_error_not_a_sample() {
    let lines = [
        lined(1, "turn_started", None, serde_json::json!({"input": []})),
        lined(
            2,
            "assistant_message_started",
            Some("a_m"),
            serde_json::json!({}),
        ),
        lined(
            3,
            "text_completed",
            Some("a_m"),
            serde_json::json!({"text": "has qqq-no-such-match inside"}),
        ),
        lined(
            4,
            "assistant_message_completed",
            Some("a_m"),
            serde_json::json!({"outcome": "completed"}),
        ),
        lined(
            5,
            "turn_completed",
            None,
            serde_json::json!({"outcome": "completed"}),
        ),
    ]
    .join("\n");
    let error = crate::measure_paging(&lines, 60, 12, fakes::clock::FakeClock::new())
        .expect_err("an accidental match must fail the jig");
    assert!(
        error.contains("matched 1"),
        "the sample names its matches: {error}"
    );
}

#[test]
fn measure_paging_on_no_events_reports_zero_searches() {
    // No lines, no session, no search: the smoke run still reports,
    // with both searches at zero rather than failing.
    let report = crate::measure_paging("", 60, 12, fakes::clock::FakeClock::new())
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        report.contains("search of the whole log: 0.00 ms, 0 matches\n"),
        "{report}"
    );
    assert!(
        report.contains("search of the whole log with no match: 0.00 ms, 0 matches\n"),
        "{report}"
    );
}

#[test]
fn searching_with_no_session_is_an_error_not_a_sample() {
    // A direct search with no session cannot open its bar: it errors
    // instead of reporting a vacuous zero-match sample.
    let lines: Vec<(contract::Seq, String)> = Vec::new();
    let clock = fakes::clock::FakeClock::new();
    let mut paging = over(&lines, clock);
    let error = paging
        .search_absent()
        .expect_err("no session means no search");
    assert!(
        error.contains("no session to search"),
        "the error says why: {error}"
    );
}

#[test]
fn a_frame_names_the_range_the_log_does_not_hold() {
    // Turns with text enough to cut a second page and drop the first;
    // the log then loses the first page, so the frame errors naming its
    // range instead of loading forever. This pins the snapshot's
    // empty-range guard: without it the frame would settle an unloadable
    // page as loaded.
    let kinds = [
        (
            "turn_started",
            None,
            serde_json::json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "go"}]}]}),
        ),
        (
            "text_completed",
            Some("a_m"),
            serde_json::json!({"text": "filler text that draws rows"}),
        ),
        (
            "turn_completed",
            None,
            serde_json::json!({"outcome": "completed"}),
        ),
    ];
    let lines: Vec<(contract::Seq, String)> = (0..25)
        .flat_map(|_| kinds.iter().cloned())
        .enumerate()
        .map(|(at, (kind, action, payload))| {
            let seq = u64::try_from(at).unwrap_or(0).saturating_add(1);
            let envelope = Envelope {
                kind: kind.to_owned(),
                session_id: contract::SessionId(SESSION.to_owned()),
                ts: 0,
                schema_version: contract::SCHEMA_VERSION,
                turn_id: None,
                action_id: action.map(|id| contract::ActionId(id.to_owned())),
                seq: Some(contract::Seq(seq)),
                payload: payload.as_object().cloned().unwrap_or_default(),
            };
            (
                contract::Seq(seq),
                serde_json::to_string(&envelope).unwrap_or_default(),
            )
        })
        .collect();
    let mut app = crate::app::App::new(None);
    app.set_size(60, 12);
    for (_, line) in &lines {
        let envelope: contract::Envelope =
            serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}"));
        if app.session().is_none() {
            app.attach(envelope.session_id.clone());
        }
        app.on_line(crate::link::Line::Session(envelope));
    }
    assert!(app.pages().part(0).is_none(), "the first page drops");
    let clock = fakes::clock::FakeClock::new();
    let end = app.pages().index().pages()[0].last_seq.0;
    let kept: Vec<(contract::Seq, &str)> = lines
        .iter()
        .filter(|(seq, _)| seq.0 > end)
        .map(|(seq, line)| (*seq, line.as_str()))
        .collect();
    assert!(!kept.is_empty(), "the log keeps the later pages");
    let mut paging = super::Paging {
        app,
        log: kept,
        area: ratatui::layout::Rect::new(0, 0, 60, 12),
        clock,
        most: 0,
    };
    // Scroll to the top, so the window needs the dropped first page
    // the log no longer holds.
    paging.app.jump(0);
    let error = paging.frame().unwrap_err();
    assert!(
        error.contains("holds no lines"),
        "the missing range errors: {error}"
    );
}

/// The paging fake answers a `history` with no `from_seq` as the doors
/// do: the last lines up to `to_seq`, or up to the latest, at most 256.
/// Exercised through `Paging::search`, which pages the fake's window:
/// a marker in the last window and one before it are each found once.
#[test]
fn the_paging_fake_answers_history_without_from_seq() {
    // Fifty turns, six lines each: 300 lines, so the first fetch holds
    // the last 256 and older lines page in behind it.
    let mut lines: Vec<(contract::Seq, String)> = Vec::new();
    for turn in 0u64..50 {
        let base = turn * 6;
        let message = format!("a_m{turn}");
        let marker = format!("zzquokka{turn}zz");
        lines.push((
            contract::Seq(base),
            lined(base, "turn_started", None, json!({"input": []})),
        ));
        lines.push((
            contract::Seq(base + 1),
            lined(
                base + 1,
                "assistant_message_started",
                Some(&message),
                json!({}),
            ),
        ));
        lines.push((
            contract::Seq(base + 2),
            lined(
                base + 2,
                "text_completed",
                Some(&message),
                json!({"text": marker}),
            ),
        ));
        lines.push((
            contract::Seq(base + 3),
            lined(
                base + 3,
                "assistant_message_completed",
                Some(&message),
                json!({"outcome": "completed"}),
            ),
        ));
        lines.push((
            contract::Seq(base + 4),
            lined(
                base + 4,
                "turn_completed",
                None,
                json!({"outcome": "completed"}),
            ),
        ));
        lines.push((
            contract::Seq(base + 5),
            numbered(base + 5, "usage_recorded"),
        ));
    }
    let app = silent_app(&lines);
    assert!(app.pages().part(0).is_none(), "the first page drops");
    let clock = fakes::clock::FakeClock::new();
    let log: Vec<(contract::Seq, &str)> = lines
        .iter()
        .map(|(seq, line)| (*seq, line.as_str()))
        .collect();
    let mut paging = super::Paging {
        app,
        log,
        area: ratatui::layout::Rect::new(0, 0, 60, 12),
        clock,
        most: 0,
    };
    // The last turn's marker sits in the first window: found exactly
    // once through the fake's absent-`from_seq` answer. The first
    // turn's marker pages in behind it through a `to_seq` fetch, so
    // the jig connects first, as the smoke run does. A fresh paging
    // per search keeps the settled state of one out of the other.
    paging
        .app
        .on_line(crate::link::Line::Hub(contract::HubLine {
            kind: "hub_hello".to_owned(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            payload: serde_json::Map::new(),
        }));
    let (_, found) = paging
        .search("zzquokka49zz")
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(found, 1, "the last window's marker");
    let app = silent_app(&lines);
    assert!(app.pages().part(0).is_none(), "the first page drops");
    let clock = fakes::clock::FakeClock::new();
    let log: Vec<(contract::Seq, &str)> = lines
        .iter()
        .map(|(seq, line)| (*seq, line.as_str()))
        .collect();
    let mut paging = super::Paging {
        app,
        log,
        area: ratatui::layout::Rect::new(0, 0, 60, 12),
        clock,
        most: 0,
    };
    paging
        .app
        .on_line(crate::link::Line::Hub(contract::HubLine {
            kind: "hub_hello".to_owned(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            payload: serde_json::Map::new(),
        }));
    let (_, found) = paging
        .search("zzquokka0zz")
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(found, 1, "the marker before the first window");
}

/// A search over a small log answers its first window from the whole
/// log: with no `from_seq` the doors hold the last lines up to `to`,
/// so every match in a settled scan is found.
#[test]
fn a_search_answers_its_first_window_from_the_whole_log() {
    let lines: Vec<(contract::Seq, String)> = vec![
        (
            contract::Seq(0),
            lined(0, "turn_started", None, json!({"input": []})),
        ),
        (
            contract::Seq(1),
            lined(1, "assistant_message_started", Some("a_m"), json!({})),
        ),
        (
            contract::Seq(2),
            lined(
                2,
                "text_completed",
                Some("a_m"),
                json!({"text": "this turn uses the shell"}),
            ),
        ),
        (
            contract::Seq(3),
            lined(
                3,
                "assistant_message_completed",
                Some("a_m"),
                json!({"outcome": "completed"}),
            ),
        ),
        (
            contract::Seq(4),
            lined(4, "turn_completed", None, json!({"outcome": "completed"})),
        ),
    ];
    let app = silent_app(&lines);
    let clock = fakes::clock::FakeClock::new();
    let log: Vec<(contract::Seq, &str)> = lines
        .iter()
        .map(|(seq, line)| (*seq, line.as_str()))
        .collect();
    let mut paging = super::Paging {
        app,
        log,
        area: ratatui::layout::Rect::new(0, 0, 60, 12),
        clock,
        most: 0,
    };
    let (_, matches) = paging.search("shell").expect("a small log settles");
    assert_eq!(matches, 1, "the first window holds the whole log");
    let (_, absent) = paging
        .search("qqq-no-such-match")
        .expect("a small log settles");
    assert_eq!(absent, 0, "an absent query matches nothing");
}

#[test]
fn hover_frames_finds_the_badge_row_at_160_by_48() {
    let events = include_str!("../examples/hover.jsonl");
    let (row, _) = crate::hover_frames(events, 160, 48, &[])
        .unwrap_or_else(|error| panic!("hover_frames: {error}"));
    let mut app = super::fold(events, 160, 48).unwrap_or_else(|error| panic!("fold: {error}"));
    app.put_aside();
    let badge = app.badge().expect("a put-aside request shows a badge");
    let area = ratatui::layout::Rect::new(0, 0, 160, 48);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    crate::view::render(&app, area, &mut buf, None);
    let line = crate::view::text(&buf)
        .lines()
        .nth(usize::from(row))
        .unwrap_or_default()
        .to_owned();
    assert!(
        line.contains(&badge),
        "row {row} shows {badge:?} in {line:?}"
    );
    let (_, bytes) = crate::hover_frames(events, 160, 48, &[(3, row), (3, 0)])
        .unwrap_or_else(|error| panic!("hover_frames: {error}"));
    assert_eq!(bytes.len(), 2);
    assert!(bytes[0] > 0);
    assert!(bytes[1] > 0);
}

/// Folding the fixture through `measure_open` into `fold_batch` makes
/// exactly one borrowed decode per `read!` the fold reaches: no line
/// decodes twice, and none is skipped.
#[test]
fn folding_the_fixture_counts_each_borrowed_decode_once() {
    let fixture = [
        ("turn_started", json!({"input": []})),
        ("turn_completed", json!({"outcome": "completed"})),
        ("text_completed", json!({"text": "t"})),
    ]
    .iter()
    .map(|(kind, payload)| {
        serde_json::to_string(&Envelope {
            kind: (*kind).to_owned(),
            session_id: contract::SessionId(SESSION.to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: None,
            seq: None,
            payload: payload.as_object().cloned().unwrap_or_default(),
        })
        .unwrap_or_default()
    })
    .collect::<Vec<_>>()
    .join("\n");
    let clock = fakes::clock::FakeClock::new();
    let before = crate::app::decode_count();
    crate::measure_open(&fixture, 60, 12, usize::MAX, clock)
        .unwrap_or_else(|error| panic!("{error}"));
    // Two reads for the started turn, one for its completion and one for
    // the completed text, whose read needs the open turn.
    assert_eq!(crate::app::decode_count() - before, 4);
}
