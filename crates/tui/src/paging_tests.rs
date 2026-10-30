//! Tests for the window of history (`docs/tui.md`, "History and paging"):
//! pages joined draw what one fold of the whole session draws, the window
//! holds the only pages kept, loading and dropping never move a row, a new
//! width counts every page again, and what the person opened survives a
//! page's reload.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::rc::Rc;

use contract::clock::Clock;
use contract::{ActionId, Envelope, Seq, SessionId};
use serde_json::{Value, json};

use super::{Draw, Folded, Pages, Part, fold};
use crate::app::{App, Effect, Target};
use crate::ext::{Failure, Handle, Input, RawSpan, Renderer};
use crate::keys::Key;
use crate::link::Line;
use crate::local_time::new_york;
use crate::rows::Rows;
use crate::turn::{Entry, Row, Turn, target_id};

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// Builds a session's stream: durable lines numbered from 0, and the
/// ephemeral deltas a live stream carries when `live`.
struct Stream {
    lines: Vec<Envelope>,
    seq: u64,
    ts: u64,
    live: bool,
}

impl Stream {
    fn new(live: bool) -> Self {
        Self {
            lines: Vec::new(),
            seq: 0,
            ts: 1_000,
            live,
        }
    }

    /// One line. A delta carries the time of the durable line it comes
    /// before, so a live and a replayed session time their items alike.
    fn line(&mut self, kind: &str, durable: bool, action: Option<&str>, payload: Value) {
        let ts = self.ts + 700;
        if durable {
            self.ts = ts;
        }
        let seq = durable.then(|| {
            self.seq += 1;
            Seq(self.seq - 1)
        });
        self.lines.push(Envelope {
            kind: kind.to_owned(),
            session_id: SessionId(SESSION.to_owned()),
            ts,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: action.map(|id| ActionId(id.to_owned())),
            seq,
            payload: payload.as_object().cloned().unwrap_or_default(),
        });
    }

    fn durable(&mut self, kind: &str, action: Option<&str>, payload: Value) {
        self.line(kind, true, action, payload);
    }

    fn ephemeral(&mut self, kind: &str, action: Option<&str>, payload: Value) {
        if self.live {
            self.line(kind, false, action, payload);
        }
    }

    /// Text, streamed in two deltas when live, then completed.
    fn text(&mut self, message: &str, text: &str) {
        let (head, tail) = text.split_at(text.len() / 2);
        self.ephemeral(
            "assistant_message_delta",
            Some(message),
            json!({"text": head}),
        );
        self.ephemeral(
            "assistant_message_delta",
            Some(message),
            json!({"text": tail}),
        );
        self.durable("text_completed", Some(message), json!({"text": text}));
    }

    /// A thinking block, streamed when live.
    fn think(&mut self, action: &str, text: &str) {
        self.durable("reasoning_started", Some(action), json!({}));
        self.ephemeral("reasoning_delta", Some(action), json!({"text": text}));
        self.durable("reasoning_completed", Some(action), json!({"text": text}));
    }

    /// A call the model emits, its arguments streamed when live.
    fn request(&mut self, message: &str, index: u32, call: &str, name: &str, path: &str) {
        let arguments = json!({"path": path});
        self.ephemeral(
            "tool_call_arguments_delta",
            Some(message),
            json!({"index": index, "name": name, "text": arguments.to_string()}),
        );
        self.durable(
            "tool_call_requested",
            Some(call),
            json!({"name": name, "arguments": arguments}),
        );
    }

    fn complete(&mut self, call: &str, edit: bool) {
        let mut payload =
            json!({"status": "completed", "content": [{"type": "text", "text": "ok"}]});
        if edit && let Some(object) = payload.as_object_mut() {
            object.insert(
                "changes".to_owned(),
                json!([{"path": "src/lib.rs", "added": 3, "removed": 1}]),
            );
        }
        self.durable("tool_call_started", Some(call), json!({}));
        self.durable("tool_call_completed", Some(call), payload);
    }

    /// A usage line for `generation`, with `extra` keys.
    fn usage(&mut self, generation: &str, tokens: u64, extra: Value) {
        let mut payload = json!({"generation_id": generation, "model": "fake/m",
            "tokens": {"input": tokens, "cache_read": 0, "cache_write": {}, "output": 40},
            "input_bytes": 0, "cost": null});
        if let (Some(payload), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
            payload.extend(extra.clone());
        }
        self.durable("usage_recorded", Some("a_m"), payload);
    }

    fn permission(&mut self, kind: &str, call: &str, request: &str) {
        self.durable(
            kind,
            Some(call),
            json!({"request_id": request, "effects": ["executes"], "reversible": true,
                "step": "standing_ask",
                "standing_rule": {"scope": "global", "prefix": "rg"},
                "decision": "allow_once"}),
        );
    }

    /// One turn of `steps` steps: thinking alone before the first reply,
    /// replies that wrap differently at different widths, tool-only steps
    /// that make a group span steps, thinking that joins an open group, a
    /// steer, an approval, an edit, a call completed after the turn's end
    /// and a usage line corrected after it.
    fn turn(&mut self, turn: usize, steps: usize) {
        self.durable(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("prompt {turn}: look at the code")}]}]}),
        );
        let mut late = None;
        for step in 0..steps {
            let id = format!("{turn}_{step}");
            let message = format!("a_m{id}");
            self.durable("step_started", None, json!({}));
            if step == 9 {
                self.durable(
                    "steering_applied",
                    None,
                    json!({"content": [{"type": "text", "text": format!("steer {turn}")}], "source": "driver"}),
                );
            }
            self.durable("assistant_message_started", Some(&message), json!({}));
            if step == 0 || step % 5 == 2 {
                self.think(&format!("a_r{id}"), &format!("**Plan {id}**\nweigh it"));
            }
            let last = step + 1 == steps;
            if last || step % 4 != 3 {
                let words = "the quick brown fox jumps over the lazy dog ".repeat(step % 5 + 1);
                self.text(&message, &format!("reply {id}: {words}"));
            }
            let calls = match (last, step) {
                (true, _) | (_, 0) => 0,
                (false, step) if step % 3 == 1 => 2,
                (false, _) => 1,
            };
            let ids: Vec<String> = (0..calls).map(|call| format!("a_t{id}_{call}")).collect();
            for (index, call) in (0u32..).zip(&ids) {
                let name = if step % 6 == 5 { "edit" } else { "read" };
                self.request(&message, index, call, name, &format!("src/{id}.rs"));
            }
            self.durable(
                "assistant_message_completed",
                Some(&message),
                json!({"outcome": "completed"}),
            );
            self.usage(&format!("g{id}"), 100, json!({}));
            for (at, call) in ids.iter().enumerate() {
                if step % 7 == 4 && at == 0 {
                    self.permission("permission_requested", call, &format!("r{id}"));
                    self.permission("permission_resolved", call, &format!("r{id}"));
                }
                if step + 2 == steps && at == 0 {
                    late = Some(call.clone());
                } else {
                    self.complete(call, step % 6 == 5);
                }
            }
        }
        self.durable("turn_completed", None, json!({"outcome": "completed"}));
        if let Some(call) = late {
            self.complete(&call, false);
        }
        self.usage(&format!("g{turn}_1"), 123_456, json!({}));
    }
}

/// A session of `turns` turns of 30 steps.
fn session(turns: usize, live: bool) -> Vec<Envelope> {
    let mut stream = Stream::new(live);
    for turn in 0..turns {
        stream.turn(turn, 30);
    }
    stream.lines
}

/// The durable lines in `range`, as `history` answers.
fn history(lines: &[Envelope], range: &RangeInclusive<Seq>) -> Vec<Envelope> {
    lines
        .iter()
        .filter(|line| line.seq.is_some_and(|seq| range.contains(&seq)))
        .cloned()
        .collect()
}

/// What one fold of every line draws, with the totals `pages` kept.
fn whole(pages: &Pages, lines: &[Envelope]) -> Vec<Row> {
    let seed = pages.seeds.first().cloned().expect("the initial page seed");
    let mut part = Part::seeded(seed);
    for line in lines {
        fold(&mut part, line);
    }
    let mut out = Rows::default();
    pages.draw(0, &part, &mut out);
    out.into_parts().0
}

/// Every page's lines joined, loading each dropped page from `lines`, and
/// checking each page's count against the rows its lines draw.
fn joined(pages: &mut Pages, lines: &[Envelope]) -> Vec<Row> {
    let mut out = Vec::new();
    for at in 0..pages.index().pages().len() {
        if pages.part(at).is_none() {
            let range = pages
                .index()
                .pages()
                .get(at)
                .map(|page| page.first_seq..=page.last_seq);
            if let Some(range) = range {
                pages.load(&SessionId(SESSION.to_owned()), &history(lines, &range));
            }
        }
        assert!(pages.part(at).is_some(), "page {at} did not load");
        let mut rows = Rows::default();
        if let Some(part) = pages.part(at) {
            pages.draw(at, part, &mut rows);
        }
        let drawn: usize = rows
            .iter()
            .map(|(line, _)| crate::view::rows(line, pages.width))
            .sum();
        let counted = pages.index().pages().get(at).map(|page| page.rows);
        assert_eq!(counted, Some(drawn), "page {at}");
        out.extend(rows.into_parts().0);
    }
    out
}

/// Where `got` first differs from `want`, line and target, if it does.
fn differs(got: &[Row], want: &[Row]) -> Option<String> {
    let at = got
        .iter()
        .zip(want)
        .position(|(got, want)| got != want)
        .or_else(|| (got.len() != want.len()).then(|| got.len().min(want.len())))?;
    let show = |rows: &[Row]| -> Vec<String> {
        rows.iter()
            .skip(at.saturating_sub(2))
            .take(5)
            .map(|(line, target)| format!("{line} {target:?}"))
            .collect()
    };
    Some(format!(
        "at line {at} of {} against {}: {:#?} against {:#?}",
        got.len(),
        want.len(),
        show(got),
        show(want)
    ))
}

/// Drops every closed page.
fn drop_all(pages: &mut Pages) {
    pages.residency.drop_all_for_test();
}

#[test]
fn pages_joined_draw_what_one_fold_draws() {
    for live in [true, false] {
        let lines = session(3, live);
        for width in [40, 120] {
            let mut pages = Pages::new(width);
            for line in &lines {
                pages.apply(line);
            }
            assert!(pages.index().pages().len() > 10, "too few pages");
            let reference = whole(&pages, &lines);
            assert!(reference.len() > 100);
            // The live fold's pages, as they stand.
            let got = joined(&mut pages, &lines);
            assert_eq!(differs(&got, &reference), None, "live {live} at {width}");
            // Every page folded again from its durable lines.
            drop_all(&mut pages);
            let got = joined(&mut pages, &lines);
            assert_eq!(
                differs(&got, &reference),
                None,
                "refolded {live} at {width}"
            );
        }
    }
}

#[test]
fn a_live_and_a_replayed_session_draw_the_same() {
    let live = session(3, true);
    let replayed = session(3, false);
    let mut one = Pages::new(60);
    let mut two = Pages::new(60);
    for line in &live {
        one.apply(line);
    }
    for line in &replayed {
        two.apply(line);
    }
    assert_eq!(differs(&one.rows(), &two.rows()), None);
    assert_eq!(one.index().pages(), two.index().pages());
}

#[test]
fn pages_are_cut_inside_turns() {
    let lines = session(2, false);
    let mut pages = Pages::new(20);
    for line in &lines {
        pages.apply(line);
    }
    let cuts = pages.seeds.iter().filter(|seed| seed.cut).count();
    assert!(cuts > 2, "{cuts} cuts inside turns");
    assert!(pages.seeds.iter().skip(1).any(|seed| seed.step.is_some()));
}

/// An app attached to the test session at `width` by `height`.
fn app(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    app.on_line(Line::Hub(hello));
    app.set_size(width, height);
    app.attach(SessionId(SESSION.to_owned()));
    app
}

/// One frame's loading: every range the app needs, from `lines`.
fn frame(app: &mut App, lines: &[Envelope]) {
    while let Some((session, range)) = app.needs().first().cloned() {
        app.load(&session, history(lines, &range));
        assert_ne!(
            app.needs().first(),
            Some(&(session, range)),
            "loading did not settle"
        );
    }
}

/// Opens `lines` as the `full` subscription sends them, a frame a line.
fn open(app: &mut App, lines: &[Envelope]) {
    for line in lines {
        app.on_line(Line::Session(line.clone()));
        frame(app, lines);
    }
}

/// Every page's first row, and every row.
fn layout(app: &App) -> (Vec<usize>, usize) {
    (app.pages().index().starts(), app.scroll().1)
}

/// Whether every resident page but the open one is in the window.
fn only_the_window(app: &App) -> bool {
    let (top, _) = app.scroll();
    let window = app.pages().index().window(top, app.conversation_height());
    let open = app.pages().index().pages().len() - 1;
    (0..open).all(|at| app.pages().part(at).is_none() || window.contains(&at))
}

fn now() -> std::time::Instant {
    fakes::clock::FakeClock::new().now()
}

#[test]
fn the_opening_pass_keeps_only_the_window() {
    let lines = session(3, true);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    let pages = app.pages().index().pages().len();
    assert!(pages > 10);
    assert!(only_the_window(&app));
    let resident = app.pages().resident();
    assert!(resident < pages / 2, "{resident} of {pages}");
    // Following: the last screenful.
    let (top, total) = app.scroll();
    assert_eq!(top, total - app.conversation_height());
}

#[test]
fn paging_to_the_top_and_back_moves_no_row() {
    let lines = session(3, true);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    let before = layout(&app);
    let mut loaded = 0;
    while app.scroll().0 > 0 {
        app.on_key(Key::PageUp, now());
        loaded += app.needs().len();
        frame(&mut app, &lines);
        assert_eq!(layout(&app), before);
        assert!(only_the_window(&app));
    }
    assert!(loaded > 0);
    assert!(app.pages().part(0).is_some());
    while app.scroll().0 + app.conversation_height() < app.scroll().1 {
        app.on_key(Key::PageDown, now());
        frame(&mut app, &lines);
        assert_eq!(layout(&app), before);
        assert!(only_the_window(&app));
    }
    assert!(app.pages().part(0).is_none());
}

#[test]
fn the_window_reaches_one_screen_either_side() {
    let lines = session(6, false);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    let height = app.conversation_height();
    let top = app.scroll().1 / 2;
    app.jump(top);
    frame(&mut app, &lines);
    let pages = app.pages().index().pages().to_vec();
    let starts = app.pages().index().starts();
    for (at, (page, start)) in pages.iter().zip(&starts).enumerate() {
        let end = start + page.rows;
        let inside = page.rows > 0 && end > top - height && *start < top + 2 * height;
        let open = at + 1 == pages.len();
        assert_eq!(app.pages().part(at).is_some(), inside || open, "page {at}");
    }
}

#[test]
fn a_new_width_counts_every_page_again() {
    let lines = session(3, true);
    let mut app = app(120, 20);
    open(&mut app, &lines);
    app.on_key(Key::PageUp, now());
    frame(&mut app, &lines);
    app.set_size(40, 20);
    assert!(app.needs().len() > 3);
    frame(&mut app, &lines);
    assert!(only_the_window(&app));
    let mut fresh = self::app(40, 20);
    open(&mut fresh, &lines);
    assert_eq!(layout(&app), layout(&fresh));
    let (top, total) = app.scroll();
    assert!(top + app.conversation_height() <= total);
    // A height change keeps every count.
    let before = layout(&app);
    app.set_size(40, 30);
    frame(&mut app, &lines);
    assert_eq!(layout(&app), before);
    assert!(only_the_window(&app));
}

#[test]
fn a_streaming_reply_on_the_open_page_is_never_dropped() {
    let mut stream = Stream::new(true);
    for turn in 0..4 {
        stream.turn(turn, 30);
    }
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go on"}]}]}),
    );
    stream.durable("step_started", None, json!({}));
    stream.durable("assistant_message_started", Some("a_mlive"), json!({}));
    stream.ephemeral(
        "assistant_message_delta",
        Some("a_mlive"),
        json!({"text": "still streaming"}),
    );
    let lines = stream.lines;
    let mut app = app(80, 20);
    open(&mut app, &lines);
    let open = app.pages().index().pages().len() - 1;
    app.jump(0);
    frame(&mut app, &lines);
    assert!(app.pages().part(0).is_some());
    assert!(app.pages().part(open).is_some());
    let texts: Vec<String> = app
        .pages()
        .rows()
        .iter()
        .map(|(line, _)| line.to_string())
        .collect();
    assert!(texts.contains(&" still streaming".to_owned()));
}

#[test]
fn refolding_leaves_session_state_alone() {
    let mut stream = Stream::new(false);
    for turn in 0..5 {
        stream.turn(turn, 30);
    }
    // A running turn with an approval waiting.
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go on"}]}]}),
    );
    stream.durable("step_started", None, json!({}));
    stream.request("a_mw", 0, "a_tw", "read", "w.rs");
    stream.permission("permission_requested", "a_tw", "r_w");
    let lines = stream.lines;
    let mut app = app(80, 20);
    open(&mut app, &lines);
    // The panel set aside, the badge says it waits.
    app.on_key(Key::Esc, now());
    let badge = app.badge();
    assert!(badge.is_some());
    app.jump(0);
    frame(&mut app, &lines);
    assert!(app.pages().part(0).is_some());
    assert_eq!(app.badge(), badge);
    assert_eq!(app.notice(), None);
    // Still busy: Enter steers.
    for ch in "more".chars() {
        app.on_key(Key::Char(ch), now());
    }
    let sent = match app.on_key(Key::Enter, now()) {
        Effect::Send(sent) => sent,
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_) => Vec::new(),
    };
    assert!(
        sent.iter().any(|line| line.contains("\"steer\"")),
        "{sent:?}"
    );
}

#[test]
fn a_failed_load_keeps_every_count_and_says_why() {
    let lines = session(6, false);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    let before = layout(&app);
    app.jump(0);
    let range = app.needs().first().cloned();
    assert!(range.is_some(), "the top was resident");
    let Some((session, range)) = range else {
        return;
    };
    app.load_failed(&session, &range, "from_seq is past the latest line");
    assert_eq!(
        app.notice(),
        Some("Could not load history: from_seq is past the latest line")
    );
    assert!(!app.needs().contains(&(session, range)));
    frame(&mut app, &lines);
    assert!(app.pages().part(0).is_none());
    assert_eq!(layout(&app), before);
    // Its rows draw blank.
    let shown = app.shown(0, 1);
    assert_eq!(shown.first, 0);
    assert_eq!(
        shown.lines.first().map(|(line, _, _)| line.to_string()),
        Some(String::new())
    );
}

/// The first group target among the resident lines.
fn first_group(app: &App) -> Option<Target> {
    app.targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, Target::Group(_)).then_some(target))
}

/// The lines of resident page `at`.
fn page_texts(app: &App, at: usize) -> Vec<String> {
    let mut rows = Rows::default();
    if let Some(part) = app.pages().part(at) {
        app.pages().draw(at, part, &mut rows);
    }
    rows.iter().map(|(line, _)| line.to_string()).collect()
}

#[test]
fn a_target_opened_stays_open_when_its_page_comes_back() {
    let lines = session(6, false);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    app.jump(0);
    frame(&mut app, &lines);
    let group = first_group(&app);
    assert!(group.is_some(), "no group on the first page");
    let Some(group) = group else {
        return;
    };
    let closed = page_texts(&app, 0).len();
    app.open(group);
    let opened = page_texts(&app, 0);
    assert!(opened.len() > closed);
    let after = layout(&app);
    // Dropped, then loaded again: still open, under the same key.
    app.on_key(Key::End, now());
    frame(&mut app, &lines);
    assert!(app.pages().part(0).is_none());
    app.jump(0);
    frame(&mut app, &lines);
    assert_eq!(page_texts(&app, 0), opened);
    assert_eq!(first_group(&app), Some(group));
    assert_eq!(layout(&app), after);
}

#[test]
fn ctrl_o_reaches_pages_loaded_later() {
    let lines = session(3, false);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    assert!(app.pages().part(0).is_none());
    app.on_key(Key::CtrlO, now());
    frame(&mut app, &lines);
    // Every page counted again with its ledgers open.
    let mut fresh = self::app(80, 20);
    fresh.on_key(Key::CtrlO, now());
    open(&mut fresh, &lines);
    assert_eq!(layout(&app), layout(&fresh));
    app.jump(0);
    frame(&mut app, &lines);
    let texts = page_texts(&app, 0);
    assert!(
        texts.iter().any(|line| line.starts_with("     2 ")),
        "{texts:?}"
    );
}

#[test]
fn a_late_usage_line_moves_only_its_turns_closing_rows() {
    let mut stream = Stream::new(false);
    for turn in 0..4 {
        stream.turn(turn, 30);
    }
    let mut app = app(30, 10);
    open(&mut app, &stream.lines);
    let before = layout(&app);
    // A correction for the first turn, whose ▣ line is long dropped.
    stream.usage(
        "g0_1",
        987_654_321,
        json!({"cost": 12.5, "subscription": true}),
    );
    let late = stream.lines.last().cloned();
    assert!(late.is_some());
    if let Some(late) = late {
        app.on_line(Line::Session(late));
    }
    frame(&mut app, &stream.lines);
    let mut fresh = self::app(30, 10);
    open(&mut fresh, &stream.lines);
    assert_eq!(layout(&app), layout(&fresh));
    assert_ne!(layout(&app).1, before.1);
}

/// A session with crash lines across its pages: a resume cuts the first
/// turn short and orphans a job, then a long turn cuts the pages whose
/// seeds would clone the fold holding that text.
fn crashed_session() -> Vec<Envelope> {
    let mut stream = Stream::new(false);
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
    stream.durable(
        "fiber_started",
        None,
        json!({"version": "0.0.1", "resumed": true}),
    );
    stream.durable(
        "job_started",
        None,
        json!({"job_id": "j_zz", "description": "zz describe the indescribable",
            "output_path": "/tmp/o"}),
    );
    stream.durable(
        "job_completed",
        None,
        json!({"job_id": "j_zz", "status": "failed",
            "error": {"code": "orphaned", "message": "zz orphaned away"}}),
    );
    stream.turn(1, 30);
    stream.lines
}

/// The joined rows of one cut case hold what the crash promises: an
/// orphaned job draws its one line naming its jobs exactly once, while a
/// job that finished, or one still running, draws no orphan line
/// (`docs/tui.md`, "After a crash").
fn check_cut(name: &str, rows: &[String], orphan: Option<&str>) {
    let notices = rows
        .iter()
        .filter(|row| row.contains("Orphaned jobs"))
        .count();
    match orphan {
        Some(want) => {
            assert_eq!(notices, 1, "{name}: {rows:?}");
            assert!(
                rows.iter().any(|row| row.contains(want)),
                "{name}: {rows:?}"
            );
        }
        None => assert_eq!(notices, 0, "{name}: {rows:?}"),
    }
}

/// One cut case's live and reloaded rows: every page joined before anything
/// is dropped, then every page dropped and loaded again from history; both
/// hold what the crash promises and the reload reads what live drew
/// (`docs/tui.md`, "History and paging").
fn check_case(name: &str, lines: &[Envelope], orphan: Option<&str>) {
    let mut pages = Pages::new(80);
    for line in lines {
        pages.apply(line);
    }
    let live = texts(&joined(&mut pages, lines));
    check_cut(name, &live, orphan);
    drop_all(&mut pages);
    let reloaded = texts(&joined(&mut pages, lines));
    assert_eq!(reloaded, live, "{name} reloads what live drew");
    check_cut(&format!("{name} reloaded"), &reloaded, orphan);
}

#[test]
fn job_descriptions_stay_on_the_page_that_holds_them() {
    // Ten cut scenarios over the same rendered-output promises: the orphan
    // line names its jobs and survives a cut and reload, a finished job
    // draws no orphan line, and the reloaded rows read what live drew.
    // Pages are cut at step boundaries and dropped pages reload from
    // history (`docs/tui.md`, "History and paging").
    let mut done = Stream::new(false);
    done.turn(0, 1);
    for index in 0..40 {
        let id = format!("j_done_{index}");
        job_started(
            &mut done,
            &id,
            &format!("completed job {index} description"),
        );
        job_done(&mut done, &id);
    }
    job_started(&mut done, "j_open", "active job description");
    done.turn(1, 1);
    let cases: Vec<(&str, Vec<Envelope>, Option<&str>)> = vec![
        (
            "the crashed session",
            crashed_session(),
            Some("Orphaned jobs: zz describe the indescribable"),
        ),
        (
            "a live cut",
            job_completed_after_page_cut(),
            Some("Orphaned jobs: build the docs"),
        ),
        (
            "a pending step cut",
            crossing_step(true),
            Some("Orphaned jobs: step over the line"),
        ),
        (
            "a confirmed step cut",
            crossing_step(false),
            Some("Orphaned jobs: step over the line"),
        ),
        (
            "a late orphan at a confirmed cut",
            orphaned_before_step_cut(),
            Some("Orphaned jobs: late to the cut"),
        ),
        (
            "a crossing orphan",
            crossing_turns(Ends::OrphanedLater),
            Some("Orphaned jobs: cross the boundary"),
        ),
        ("a crossing success", crossing_turns(Ends::DoneLater), None),
        (
            "a same-page orphan",
            crossing_turns(Ends::OrphanedHere),
            Some("Orphaned jobs: cross the boundary"),
        ),
        ("forty done plus one open", done.lines, None),
    ];
    for (name, lines, orphan) in cases {
        check_case(name, &lines, orphan);
    }
    // Open jobs over one, three and six cuts draw no orphan line, live or
    // reloaded.
    for cuts in [1, 3, 6] {
        let mut stream = Stream::new(false);
        stream.turn(0, 1);
        for job in 0..5 {
            job_started(&mut stream, &format!("j_{job}"), &format!("open job {job}"));
        }
        for turn in 0..cuts {
            pad(&mut stream);
            stream.turn(turn + 1, 1);
        }
        let name = format!("open jobs over {cuts} cuts");
        check_case(&name, &stream.lines, None);
    }
    // A reloaded orphan line still opens to its jobs' messages
    // (`docs/tui.md`, "After a crash").
    let lines = crashed_session();
    let mut app = app(80, 20);
    open(&mut app, &lines);
    app.jump(0);
    frame(&mut app, &lines);
    let orphan = app
        .targets()
        .into_iter()
        .find_map(|(_, target)| matches!(target, Target::Orphans(_)).then_some(target))
        .expect("a reloaded orphan target");
    app.open(orphan);
    let shown: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    assert!(
        shown.iter().any(|line| line.contains("zz orphaned away")),
        "{shown:?}"
    );
}

/// An open job crosses the cut between the first two turns, then becomes
/// orphaned before the next page closes.
fn job_completed_after_page_cut() -> Vec<Envelope> {
    let mut stream = Stream::new(false);
    stream.turn(0, 1);
    stream.durable(
        "job_started",
        None,
        json!({"job_id": "j_build", "description": "build the docs", "output_path": "/tmp/o"}),
    );
    for _ in 0..64 {
        stream.durable("unknown", None, json!({}));
    }
    stream.turn(1, 1);
    stream.durable(
        "job_completed",
        None,
        json!({"job_id": "j_build", "status": "failed",
            "error": {"code": "orphaned", "message": "j_build orphaned."}}),
    );
    stream.turn(2, 1);
    stream.lines
}

/// A job's lines: started with `description`, or completed as orphaned or
/// successfully.
fn job_started(stream: &mut Stream, id: &str, description: &str) {
    stream.durable(
        "job_started",
        None,
        json!({"job_id": id, "description": description, "output_path": "/tmp/o"}),
    );
}

fn job_orphaned(stream: &mut Stream, id: &str) {
    stream.durable(
        "job_completed",
        None,
        json!({"job_id": id, "status": "failed",
            "error": {"code": "orphaned", "message": format!("{id} orphaned.")}}),
    );
}

fn job_done(stream: &mut Stream, id: &str) {
    stream.durable(
        "job_completed",
        None,
        json!({"job_id": id, "status": "completed"}),
    );
}

/// Enough lines that the next turn cuts a page.
fn pad(stream: &mut Stream) {
    for _ in 0..64 {
        stream.durable("unknown", None, json!({}));
    }
}

/// How a job that `ended` ends.
#[derive(Clone, Copy)]
enum Ends {
    /// Orphaned on the page after the one it started on.
    OrphanedLater,
    /// Completed successfully on the page after.
    DoneLater,
    /// Orphaned on the page it started on.
    OrphanedHere,
}

/// Job `j_cross` started and ending as `ends` across turn cuts; the page
/// holding its completion closes before the session ends.
fn crossing_turns(ends: Ends) -> Vec<Envelope> {
    let mut stream = Stream::new(false);
    stream.turn(0, 1);
    if !matches!(ends, Ends::OrphanedHere) {
        job_started(&mut stream, "j_cross", "cross the boundary");
    }
    pad(&mut stream);
    stream.turn(1, 1);
    match ends {
        Ends::OrphanedLater => job_orphaned(&mut stream, "j_cross"),
        Ends::DoneLater => job_done(&mut stream, "j_cross"),
        Ends::OrphanedHere => {
            job_started(&mut stream, "j_cross", "cross the boundary");
            job_orphaned(&mut stream, "j_cross");
        }
    }
    pad(&mut stream);
    stream.turn(2, 1);
    stream.lines
}

/// Job `j_step` crosses a step cut inside one turn, orphaned while the cut
/// is pending when `pending`, else once it is confirmed; the page holding
/// the completion closes before the session ends.
fn crossing_step(pending: bool) -> Vec<Envelope> {
    let mut stream = Stream::new(false);
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
    job_started(&mut stream, "j_step", "step over the line");
    pad(&mut stream);
    stream.durable("step_started", None, json!({}));
    stream.durable("assistant_message_started", Some("a_m1"), json!({}));
    if pending {
        job_orphaned(&mut stream, "j_step");
    }
    stream.text("a_m1", "the reply that opens the step");
    stream.durable(
        "assistant_message_completed",
        Some("a_m1"),
        json!({"outcome": "completed"}),
    );
    if !pending {
        job_orphaned(&mut stream, "j_step");
    }
    stream.durable("turn_completed", None, json!({"outcome": "completed"}));
    pad(&mut stream);
    stream.turn(2, 1);
    stream.lines
}

/// Job `j_late` starts on the first page and is orphaned on the second,
/// which a confirmed step cut closes.
fn orphaned_before_step_cut() -> Vec<Envelope> {
    let mut stream = Stream::new(false);
    stream.turn(0, 1);
    job_started(&mut stream, "j_late", "late to the cut");
    pad(&mut stream);
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
    job_orphaned(&mut stream, "j_late");
    pad(&mut stream);
    stream.durable("step_started", None, json!({}));
    stream.durable("assistant_message_started", Some("a_m1"), json!({}));
    stream.text("a_m1", "the reply that opens the step");
    stream.durable(
        "assistant_message_completed",
        Some("a_m1"),
        json!({"outcome": "completed"}),
    );
    stream.durable("turn_completed", None, json!({"outcome": "completed"}));
    stream.lines
}

/// The drawn lines' texts.
fn texts(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|(line, _)| line.to_string()).collect()
}

/// A turn the process leaves suspended on one page and resumes on the
/// next, with a page cut between the exit and the resume.
fn suspended_session() -> Vec<Envelope> {
    let mut stream = Stream::new(false);
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
    stream.durable("step_started", None, json!({}));
    for step in 0..70 {
        stream.text(&format!("a_m{step}"), "hello there");
    }
    stream.durable(
        "fiber_exited",
        None,
        json!({"exit_code": 0, "suspended_on": "r_1", "usage": {"tokens":
            {"input": 0, "cache_read": 0, "cache_write": {}, "output": 0},
            "cost": 0, "subscription_cost": 0}}),
    );
    stream.durable("step_started", None, json!({}));
    stream.text("a_mcut", "after the cut");
    stream.durable(
        "fiber_started",
        None,
        json!({"version": "0.0.1", "resumed": true}),
    );
    for step in 70..75 {
        stream.text(&format!("a_m{step}"), "still going");
    }
    stream.durable("turn_completed", None, json!({"outcome": "completed"}));
    stream.lines
}

#[test]
fn a_suspended_resume_across_pages_reloads_unchanged() {
    let lines = suspended_session();
    let mut pages = Pages::new(80);
    for line in &lines {
        pages.apply(line);
    }
    let live = pages.rows();
    // The turn resumed: no cut-short card and no resume band.
    let texts: Vec<String> = live.iter().map(|(line, _)| line.to_string()).collect();
    assert!(
        !texts
            .iter()
            .any(|line| line.starts_with("▣ cut short") || line.starts_with("↺")),
        "{texts:?}"
    );
    drop_all(&mut pages);
    let got = joined(&mut pages, &lines);
    assert_eq!(differs(&got, &live), None);
}

/// A handoff whose sizing preamble stands pages before it starts.
fn handoff_session() -> Vec<Envelope> {
    let mut stream = Stream::new(false);
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
    stream.durable(
        "preamble_built",
        None,
        json!({"reason": "start", "model": "fake/m", "context_window": 1_000_000,
            "tool_choice": "auto", "cache_lifetime": "5m", "system_prompt": "",
            "tools": [], "trigger_at": 400_000}),
    );
    stream.durable("step_started", None, json!({}));
    for step in 0..70 {
        stream.text(&format!("a_m{step}"), "hello there");
    }
    stream.durable("step_started", None, json!({}));
    stream.text("a_mcut", "after the cut");
    stream.durable("handoff_started", None, json!({"trigger": "auto"}));
    for step in 70..75 {
        stream.text(&format!("a_m{step}"), "still going");
    }
    stream.durable("turn_completed", None, json!({"outcome": "completed"}));
    stream.lines
}

#[test]
fn a_handoff_sized_pages_earlier_reloads_unchanged() {
    let lines = handoff_session();
    let mut pages = Pages::new(80);
    for line in &lines {
        pages.apply(line);
    }
    let live = pages.rows();
    // The band read the trigger the preamble set pages earlier, under the
    // same target id a reload must keep.
    let texts: Vec<String> = live.iter().map(|(line, _)| line.to_string()).collect();
    assert!(texts.iter().any(|line| line.contains("400k")), "{texts:?}");
    drop_all(&mut pages);
    let got = joined(&mut pages, &lines);
    assert_eq!(differs(&got, &live), None);
}

#[test]
fn reloading_keeps_the_live_durations_and_counts() {
    let mut lines = session(1, true);
    // Production deltas arrive before the durable lines they announce, so
    // the live fold times groups from the deltas while a reload times them
    // from the requests: backdate the argument deltas past a duration
    // boundary, where the longer text wraps rows the shorter one does not.
    for line in &mut lines {
        if line.kind == "tool_call_arguments_delta" {
            line.ts = line.ts.saturating_sub(61_000);
        }
    }
    for width in [16, 20, 24, 30, 40] {
        let mut pages = Pages::new(width);
        for line in &lines {
            pages.apply(line);
        }
        assert!(pages.index().pages().len() > 2, "too few pages");
        let live = pages.rows();
        let before = (pages.index().starts(), pages.index().total());
        drop_all(&mut pages);
        let got = joined(&mut pages, &lines);
        assert_eq!(differs(&got, &live), None, "at width {width}");
        assert_eq!(
            (pages.index().starts(), pages.index().total()),
            before,
            "at width {width}"
        );
    }
}

#[test]
fn close_keeps_the_open_page_as_a_closed_one() {
    let mut pages = Pages::new(20);
    pages.close();
    // The open page closed, and a new open page stands behind it.
    assert!(pages.part(0).is_some());
    assert!(pages.part(1).is_some());
    assert_eq!(pages.resident(), 2);
}

/// Whether each resident group holding a ledger is open, one per group.
fn ledgers_open(pages: &Pages) -> Vec<bool> {
    let mut open = Vec::new();
    for at in 0..pages.index().pages().len() {
        if let Some(part) = pages.part(at) {
            open.extend(
                part.turns
                    .iter()
                    .flat_map(|card| card.groups())
                    .filter(|group| group.has_ledger())
                    .map(|group| group.open),
            );
        }
    }
    open
}

#[test]
fn toggling_ledgers_twice_closes_them_again() {
    let mut stream = Stream::new(false);
    stream.turn(0, 3);
    let mut pages = Pages::new(20);
    for line in &stream.lines {
        pages.apply(line);
    }
    let open = ledgers_open(&pages);
    assert!(!open.is_empty(), "no ledger to toggle");
    assert!(open.iter().all(|open| !open));
    pages.toggle_ledgers();
    assert!(ledgers_open(&pages).iter().all(|open| *open));
    // Every ledger stood open: closing them all shuts every one.
    pages.toggle_ledgers();
    assert!(ledgers_open(&pages).iter().all(|open| !open));
}

#[test]
fn shown_starts_at_the_first_page_drawing_a_row() {
    let lines = session(6, false);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    let starts = app.pages().index().starts();
    let at = starts
        .iter()
        .enumerate()
        .skip(1)
        .position(|(at, _)| {
            app.pages()
                .index()
                .pages()
                .get(at)
                .is_some_and(|page| page.rows > 0)
        })
        .map(|offset| offset + 1)
        .expect("a second page drawing rows");
    let top = starts.get(at).copied().expect("a second page start");
    // The page before ends exactly at the top: it draws no row from there.
    let shown = app.shown(top, 5);
    assert_eq!(shown.first, top);
    assert!(!shown.lines.is_empty());
}

#[test]
fn resident_counts_every_held_page() {
    let lines = session(2, false);
    let mut pages = Pages::new(20);
    for line in &lines {
        pages.apply(line);
    }
    // Live pages are never dropped, so the open page stands beside more
    // than one closed one: neither zero nor one matches this count.
    let total = pages.index().pages().len();
    assert!(total > 2, "{total}");
    assert_eq!(pages.resident(), total);
}

/// One envelope of `kind` with `payload`, outside any page cut.
fn envelope(kind: &str, action: Option<&str>, payload: Value) -> Envelope {
    Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: 1_000,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|id| ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// An empty page.
fn part() -> Part {
    Part {
        first: 0,
        turns: Vec::new(),
        fold: crate::turn::Fold::default(),
        aside_start: 0,
        joins_next: false,
        joins_previous: false,
    }
}

/// A page with one running turn.
fn running_part() -> Part {
    let mut page = part();
    let started = fold(
        &mut page,
        &envelope(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "hi"}]}]}),
        ),
    );
    assert!(matches!(started, Folded::Started), "{started:?}");
    page
}

#[test]
fn a_turn_started_with_no_input_folds_to_nothing() {
    // A line that folds nothing is not a new card: `if true` would start one.
    let mut page = part();
    let folded = fold(&mut page, &envelope("turn_started", None, json!({})));
    assert!(matches!(folded, Folded::Nothing), "{folded:?}");
    assert!(page.turns.is_empty());
}

#[test]
fn turn_completed_folds_to_ended_only_in_a_running_turn() {
    let done = json!({"outcome": "completed"});
    // In the running turn the line closes its card...
    let mut page = running_part();
    let folded = fold(&mut page, &envelope("turn_completed", None, done.clone()));
    assert!(matches!(folded, Folded::Ended(_, true)), "{folded:?}");
    // ...with no running turn it closes nothing...
    let mut page = part();
    let folded = fold(&mut page, &envelope("turn_completed", None, done));
    assert!(matches!(folded, Folded::Nothing), "{folded:?}");
    // ...and a line that folds nothing ends nothing either.
    let mut page = running_part();
    let folded = fold(&mut page, &envelope("turn_completed", None, json!({})));
    assert!(matches!(folded, Folded::Nothing), "{folded:?}");
}

#[test]
fn step_started_folds_to_stepped_only_in_a_running_turn() {
    let mut page = running_part();
    let folded = fold(&mut page, &envelope("step_started", None, json!({})));
    assert!(matches!(folded, Folded::Stepped), "{folded:?}");
    let mut page = part();
    let folded = fold(&mut page, &envelope("step_started", None, json!({})));
    assert!(matches!(folded, Folded::Nothing), "{folded:?}");
}

#[test]
fn a_call_request_that_folds_nothing_folds_to_nothing() {
    // An unreadable request joins no running turn: `if true` would call one.
    let mut page = running_part();
    let folded = fold(
        &mut page,
        &envelope("tool_call_requested", Some("a_c"), json!({})),
    );
    assert!(matches!(folded, Folded::Nothing), "{folded:?}");
}

/// A resumed `fiber_started`.
fn resumed(resumed: bool) -> Envelope {
    envelope(
        "fiber_started",
        None,
        json!({"version": "0.0.1", "resumed": resumed}),
    )
}

#[test]
fn a_resumed_fiber_started_cuts_the_running_card_short() {
    let mut page = running_part();
    let folded = fold(&mut page, &resumed(true));
    assert!(matches!(folded, Folded::CutShort), "{folded:?}");
    assert!(
        !page.turns.last().is_some_and(|turn| turn.is_open()),
        "the card still runs"
    );
}

#[test]
fn a_fresh_or_suspended_fiber_started_closes_no_card() {
    // A fresh process finds no open turn: `if true` would cut one short.
    let mut page = part();
    let folded = fold(&mut page, &resumed(true));
    assert!(matches!(folded, Folded::Nothing), "{folded:?}");

    // A suspended resume leaves the running card alone: it does not fold a
    // cut even though a turn was open before the line.
    let mut page = running_part();
    let exited = envelope(
        "fiber_exited",
        None,
        json!({"exit_code": 0, "usage": {"tokens": {"input": 0, "cache_read": 0,
            "cache_write": {}, "output": 0}, "cost": 0, "subscription_cost": 0},
            "suspended_on": "r_1"}),
    );
    fold(&mut page, &exited);
    let folded = fold(&mut page, &resumed(true));
    assert!(matches!(folded, Folded::Nothing), "{folded:?}");
    assert!(
        page.turns.last().is_some_and(|turn| turn.is_open()),
        "the card closed"
    );
}

#[test]
fn reloading_an_orphan_page_keeps_its_seeded_target_ids() {
    let mut stream = Stream::new(false);
    stream.durable(
        "job_completed",
        None,
        json!({"job_id": "j_1", "status": "failed",
            "error": {"code": "orphaned", "message": "lost one"}}),
    );
    stream.durable(
        "fiber_started",
        None,
        json!({"version": "0.0.1", "resumed": true}),
    );
    stream.durable(
        "job_completed",
        None,
        json!({"job_id": "j_2", "status": "failed",
            "error": {"code": "orphaned", "message": "lost two"}}),
    );
    for _ in 0..61 {
        stream.durable("unknown", None, json!({}));
    }
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "next"}]}]}),
    );

    let mut pages = Pages::new(80);
    for line in &stream.lines {
        pages.apply(line);
    }
    assert_eq!(pages.index().pages().len(), 2);
    pages.seeds.first_mut().expect("first page seed").next = 40;

    drop_all(&mut pages);
    pages.load(&SessionId(SESSION.to_owned()), &stream.lines);

    let part = pages.part(0).expect("reloaded first page");
    let mut rows = Rows::default();
    pages.draw(0, part, &mut rows);
    let rows = rows.into_parts().0;
    let targets: Vec<Target> = rows.into_iter().filter_map(|(_, target)| target).collect();
    assert_eq!(targets, [Target::Orphans(40), Target::Orphans(41)]);
}

#[test]
fn a_resumed_crash_leaves_the_session_idle() {
    let input = json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "hi"}]}]});
    let mut app = app(80, 20);
    app.on_line(Line::Session(envelope("turn_started", None, input)));
    app.on_line(Line::Session(resumed(true)));
    let lines: Vec<String> = app.lines().iter().map(ToString::to_string).collect();
    assert!(
        lines
            .iter()
            .any(|line| line.trim_start().starts_with("▣ cut short")),
        "{lines:?}"
    );
    // Not busy: Enter sends `prompt`, where `steer` would go to a turn
    // still running.
    for ch in "again".chars() {
        app.on_key(Key::Char(ch), now());
    }
    let sent = match app.on_key(Key::Enter, now()) {
        Effect::Send(sent) => sent,
        Effect::None
        | Effect::Quit
        | Effect::ListFiles
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::FindPause { .. }
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Exit(_)
        | Effect::Copy(_)
        | Effect::OpenLink(_)
        | Effect::OpenFile(_) => panic!("expected a sent line"),
    };
    assert_eq!(sent.len(), 1);
    let command: Value = serde_json::from_str(&sent[0]).unwrap_or_default();
    assert_eq!(command.get("command"), Some(&json!("prompt")), "{command}");
}

#[test]
fn only_a_changed_or_cut_line_counts_its_page_again() {
    // A line that changes a card counts its page...
    let mut pages = Pages::new(80);
    let input = json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "hi"}]}]});
    let started = pages.apply(&envelope("turn_started", None, input));
    assert!(started.changed);
    assert_eq!(pages.recounts, 1);
    // ...a step that changes nothing does not...
    let stepped = pages.apply(&envelope("step_started", None, json!({})));
    assert!(!stepped.changed);
    assert_eq!(pages.recounts, 1);
    // ...nor does a usage line for the running turn.
    let mut stream = Stream::new(false);
    stream.usage("g0", 100, json!({}));
    let used = pages.apply(&stream.lines[0]);
    assert!(used.changed);
    assert_eq!(pages.recounts, 1);
}

#[test]
fn a_step_cutting_a_page_counts_it() {
    // Replies past a page of lines fill the open page, so the next step
    // starts one: the step changes no card, and only the cut counts it.
    let mut stream = Stream::new(false);
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "hi"}]}]}),
    );
    for step in 0..70 {
        stream.text(&format!("a_m{step}"), "hello");
    }
    stream.durable("step_started", None, json!({}));
    let mut pages = Pages::new(80);
    let last = stream.lines.len() - 1;
    for line in &stream.lines[..last] {
        pages.apply(line);
    }
    let before = pages.recounts;
    let stepped = pages.apply(&stream.lines[last]);
    assert!(!stepped.changed);
    assert_eq!(pages.recounts - before, 1);
}

#[test]
fn the_open_page_caches_only_its_lines_too_wide_for_one_row() {
    use std::collections::HashMap;
    let lines = session(2, false);
    let mut pages = Pages::new(20);
    for line in &lines {
        pages.apply(line);
    }
    // A long line outside any card stays unwrapped, so the open page
    // caches it: the nudge is one line, however long. (A failed MCP
    // server's warning wraps at draw, so it caches nothing.)
    pages.apply(&Envelope {
        kind: "context_nudged".to_owned(),
        session_id: SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: json!({"tokens": 268_000, "trigger_at": 400_000})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    });
    assert!(!pages.wrapped.is_empty());
    // Exactly the open page's lines wider than the screen are cached, with
    // the rows drawing them takes.
    let width = 20u16;
    let at = pages.residency.len();
    let part = pages.part(at).expect("open page");
    let mut out = Rows::default();
    pages.draw(at, part, &mut out);
    let out = out.into_parts().0;
    let mut expected = HashMap::new();
    for (line, _) in &out {
        if line.width() > usize::from(width) {
            expected.insert(line.to_string(), crate::view::rows(line, width));
        }
    }
    assert!(!expected.is_empty());
    assert_eq!(pages.wrapped, expected);
    // The boundary: a line exactly as wide as the screen draws in one row
    // itself, so it is never cached.
    assert!(
        out.iter()
            .any(|(line, _)| line.width() == usize::from(width))
    );
    for (line, _) in &out {
        if line.width() == usize::from(width) {
            assert!(!pages.wrapped.contains_key(&line.to_string()));
        }
    }
    // A closed page counts straight from its cards and leaves the cache
    // it does not own alone.
    let before = pages.wrapped.clone();
    assert!(pages.part(0).is_some());
    pages.count(0);
    assert_eq!(pages.wrapped, before);
}

#[test]
fn shell_output_draws_only_on_its_own_page_after_its_own_turn() {
    let mut pages = Pages::new(80);
    for (page, after, text) in [(0, 0, "here"), (1, 0, "other page"), (0, 1, "later turn")] {
        pages
            .shell_index
            .entry((page, after))
            .or_default()
            .push(pages.shells.len());
        pages.shells.push((page, after, text.to_owned()));
    }
    let part = pages.part(0).expect("open page");
    let mut out = Rows::default();
    crate::work::take();
    pages.draw(0, part, &mut out);
    let out = out.into_parts().0;
    let texts: Vec<String> = out.iter().map(|(line, _)| line.to_string()).collect();
    assert_eq!(texts, vec!["here".to_owned()]);
    // Only the drawn slot's bucket is examined: one candidate, not
    // every shell the session ever ran.
    assert_eq!(crate::work::take().shell_visits, 1);
}

#[test]
fn set_opens_an_aside_only_past_the_turns() {
    // No turn holds the target, so only the page's asides can answer:
    // without the negation the aside below never opens.
    let mut page = part();
    page.fold.asides.push((
        0,
        crate::turn::crash::Aside::Orphans {
            id: 7,
            jobs: vec![("job".to_owned(), "lost".to_owned())],
            open: false,
        },
    ));
    super::set(&mut page, &Target::Orphans(7), true);
    assert!(
        page.fold.asides.iter().any(|(_, aside)| matches!(
            aside,
            crate::turn::crash::Aside::Orphans { open: true, .. }
        ))
    );
    // Another line's target changes nothing.
    super::set(&mut page, &Target::Orphans(8), false);
    assert!(
        page.fold.asides.iter().any(|(_, aside)| matches!(
            aside,
            crate::turn::crash::Aside::Orphans { open: true, .. }
        ))
    );
}

#[test]
fn draw_data_skips_a_turn_drawing_no_rows() {
    let pages = Pages::new(80);
    let mut empty = part();
    empty.turns.push(Turn::part(0));
    let (rows, _, _, turns) = pages.draw_data(0, &empty, Draw::Shown);
    assert!(rows.is_empty());
    assert!(
        turns.is_empty(),
        "a turn with no rows leaves a range: {turns:?}"
    );
}

#[test]
fn toggling_ledgers_clears_only_group_overrides() {
    let mut pages = Pages::new(80);
    pages.overrides.insert(Target::Group(1), true);
    pages.overrides.insert(Target::Thought(2), false);
    pages.toggle_ledgers();
    assert!(!pages.overrides.contains_key(&Target::Group(1)));
    assert!(pages.overrides.contains_key(&Target::Thought(2)));
}

#[test]
fn clearing_keeps_the_zone() {
    let mut pages = Pages::new(80);
    assert_eq!(pages.zone, jiff::tz::TimeZone::UTC);
    pages.zone = new_york();
    pages.clear();
    assert_eq!(pages.zone, new_york());
}

#[test]
fn a_reloaded_page_shows_the_same_time_row_as_live() {
    let mut stream = Stream::new(false);
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
    stream.durable("turn_completed", None, json!({"outcome": "completed"}));
    let lines = stream.lines;
    let zone = new_york();
    let mut pages = Pages::new(80);
    pages.zone = zone.clone();
    for line in &lines {
        pages.apply(line);
    }
    let live = pages.rows();
    // The turn started 1,700 milliseconds after the epoch.
    let time = crate::local_time::time_of_day(1700, &zone).expect("a time");
    assert_eq!(
        texts(&live),
        vec![
            "".to_owned(),
            "▄▄▄▄▄".to_owned(),
            " go ▐".to_owned(),
            "▀▀▀▀▀".to_owned(),
            format!("you {time} "),
            "".to_owned(),
            "▄".repeat(80),
            " ▣ completed".to_owned(),
            "▀".repeat(80),
        ]
    );
    drop_all(&mut pages);
    let reloaded = joined(&mut pages, &lines);
    assert_eq!(differs(&reloaded, &live), None);
}

#[test]
fn every_row_of_the_fixture_places_inside_the_rows_it_draws() {
    let lines = session(1, false);
    for width in [12, 40, 120] {
        let mut pages = Pages::new(width);
        for line in &lines {
            pages.apply(line);
        }
        for (line, _) in joined(&mut pages, &lines) {
            let rows = crate::view::rows(&line, width);
            for placed in crate::cells::place(&line, width) {
                assert!(
                    usize::from(placed.row) < rows,
                    "{line:?} at {width}: {placed:?}"
                );
                assert!(
                    placed.col.saturating_add(placed.width) <= width,
                    "{line:?} at {width}: {placed:?}"
                );
            }
        }
    }
}

/// A page with one turn holding a closed group (a thought and a call with
/// detail), a handoff note and an orphan list, all closed.
fn closed_sections() -> Part {
    let mut page = part();
    for (kind, action, payload) in [
        (
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "hi"}]}]}),
        ),
        ("reasoning_started", Some("a_r1"), json!({})),
        (
            "reasoning_completed",
            Some("a_r1"),
            json!({"text": "hidden thought"}),
        ),
        (
            "tool_call_requested",
            Some("a_t1"),
            json!({"name": "read", "arguments": {"path": "src/a.rs"}}),
        ),
        (
            "tool_call_completed",
            Some("a_t1"),
            json!({"status": "completed", "content": [{"type": "text", "text": "file words"}]}),
        ),
        ("text_completed", Some("a_m1"), json!({"text": "reply one"})),
        ("handoff_started", None, json!({"trigger": "auto"})),
        (
            "text_completed",
            Some("a_m2"),
            json!({"text": "hook note words"}),
        ),
        (
            "handoff_completed",
            None,
            json!({"outcome": "completed", "tokens_before": 10}),
        ),
        ("turn_completed", None, json!({"outcome": "completed"})),
    ] {
        fold(&mut page, &envelope(kind, action, payload));
    }
    page.fold.asides.push((
        0,
        crate::turn::crash::Aside::Orphans {
            id: 9,
            jobs: vec![("job".to_owned(), "lost".to_owned())],
            open: false,
        },
    ));
    page
}

/// The drawn rows' texts.
fn drawn_texts(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|(line, _)| line.to_string()).collect()
}

#[test]
fn all_open_draws_every_ledger_thought_call_note_and_orphan_list() {
    let pages = Pages::new(80);
    let page = closed_sections();
    let (shown, shown_texts, _, _) = pages.draw_data(0, &page, Draw::Shown);
    let (all, all_texts, _, _) = pages.draw_data(0, &page, Draw::AllOpen);
    let shown_strings = drawn_texts(&shown);
    let all_strings = drawn_texts(&all);
    assert!(all.len() > shown.len(), "an all-open draw adds rows");
    for needle in ["hidden thought", "hook note words", "lost"] {
        assert!(
            all_strings.iter().any(|line| line.contains(needle)),
            "{needle:?} is not drawn all open: {all_strings:?}"
        );
        assert!(
            shown_strings.iter().all(|line| !line.contains(needle)),
            "{needle:?} shows while closed: {shown_strings:?}"
        );
    }
    // One row per section kind names its scopes.
    let scoped: Vec<Vec<Target>> = all_texts.iter().map(|text| text.scopes.clone()).collect();
    for holds in [
        |scopes: &Vec<Target>| scopes.iter().any(|scope| matches!(scope, Target::Group(_))),
        |scopes: &Vec<Target>| {
            scopes
                .iter()
                .any(|scope| matches!(scope, Target::Thought(_)))
        },
        |scopes: &Vec<Target>| scopes.iter().any(|scope| matches!(scope, Target::Call(_))),
        |scopes: &Vec<Target>| scopes.iter().any(|scope| matches!(scope, Target::Note(_))),
        |scopes: &Vec<Target>| {
            scopes
                .iter()
                .any(|scope| matches!(scope, Target::Orphans(_)))
        },
    ] {
        assert!(scoped.iter().any(holds), "no row in {scoped:?}");
    }
    // Nothing shown names a section: every body is closed.
    assert!(
        shown_texts.iter().all(|text| text.scopes.is_empty()),
        "a shown row names a section: {shown_texts:?}"
    );
}

#[test]
fn scopes_name_every_section_a_row_is_inside() {
    let pages = Pages::new(80);
    let page = closed_sections();
    let (_, texts, _, _) = pages.draw_data(0, &page, Draw::AllOpen);
    let scopes: Vec<Vec<Target>> = texts.into_iter().map(|text| text.scopes).collect();
    // A call's detail is inside its group, outermost first; a thought
    // inside its group; the note and the orphan list stand alone.
    let detail = scopes
        .iter()
        .find(|scopes| matches!(scopes.as_slice(), [Target::Group(_), Target::Call(_)]))
        .expect("no call detail row");
    assert!(matches!(detail[0], Target::Group(_)));
    assert!(
        scopes
            .iter()
            .any(|scopes| matches!(scopes.as_slice(), [Target::Group(_), Target::Thought(_)])),
        "no thought row: {scopes:?}"
    );
    assert!(
        scopes
            .iter()
            .any(|scopes| matches!(scopes.as_slice(), [Target::Note(_)])),
        "no note row: {scopes:?}"
    );
    assert!(
        scopes
            .iter()
            .any(|scopes| matches!(scopes.as_slice(), [Target::Orphans(_)])),
        "no orphan row: {scopes:?}"
    );
}

#[test]
fn fold_text_keeps_no_page_resident() {
    let lines = session(2, false);
    let mut pages = Pages::new(80);
    for line in &lines {
        pages.apply(line);
    }
    assert!(pages.index().pages().len() > 1);
    let range = pages
        .index()
        .pages()
        .first()
        .map(|page| page.first_seq..=page.last_seq)
        .expect("a first page");
    drop_all(&mut pages);
    assert!(pages.part(0).is_none());
    let (rows, texts) = pages
        .fold_text(0, &history(&lines, &range))
        .expect("the dropped page folds");
    assert!(!rows.is_empty());
    assert_eq!(rows.len(), texts.len());
    // Kept nowhere: the page is still dropped.
    assert!(pages.part(0).is_none());
    // What it folds is what loading it keeps, drawn all open.
    pages.load(&SessionId(SESSION.to_owned()), &history(&lines, &range));
    assert_eq!(
        pages.page_text_open(0).map(|(rows, _)| drawn_texts(&rows)),
        Some(drawn_texts(&rows))
    );
    // The open page and an unknown one fold nothing.
    let open = pages.page_count().saturating_sub(1);
    assert!(pages.fold_text(open, &[]).is_none());
    assert!(pages.fold_text(open.saturating_add(1), &[]).is_none());
}

#[test]
fn force_open_on_a_dropped_page_opens_it_when_it_loads() {
    let lines = session(2, false);
    let mut pages = Pages::new(80);
    for line in &lines {
        pages.apply(line);
    }
    let group = pages
        .page_text(0)
        .and_then(|(rows, _)| {
            rows.into_iter().find_map(|(_, target)| match target {
                Some(group @ Target::Group(_)) => Some(group),
                _ => None,
            })
        })
        .expect("no group on the first page");
    let range = pages
        .index()
        .pages()
        .first()
        .map(|page| page.first_seq..=page.last_seq)
        .expect("a first page");
    drop_all(&mut pages);
    // Nothing resident holds it, so nothing changes yet: the override
    // loads it open.
    assert!(!pages.force_open(&group));
    pages.load(&SessionId(SESSION.to_owned()), &history(&lines, &range));
    let opened = pages.page_text(0).map(|(rows, _)| drawn_texts(&rows));
    assert!(
        opened.is_some_and(|texts| texts.iter().any(|line| line.contains("read"))),
        "the ledger is not open after loading"
    );
}

#[test]
fn force_open_never_closes() {
    let lines = session(2, false);
    let mut pages = Pages::new(80);
    for line in &lines {
        pages.apply(line);
    }
    let group = pages
        .page_text(0)
        .and_then(|(rows, _)| {
            rows.into_iter().find_map(|(_, target)| match target {
                Some(group @ Target::Group(_)) => Some(group),
                _ => None,
            })
        })
        .expect("no group on the first page");
    let before = pages.page_text(0).map(|(rows, _)| drawn_texts(&rows));
    assert!(pages.open(&group));
    let opened = pages.page_text(0).map(|(rows, _)| drawn_texts(&rows));
    assert_ne!(before, opened, "opening changed nothing");
    // Forcing an open section open changes nothing drawn.
    assert!(pages.force_open(&group));
    assert_eq!(
        pages.page_text(0).map(|(rows, _)| drawn_texts(&rows)),
        opened
    );
    // Nothing holds the login line here.
    assert!(!pages.force_open(&Target::Login));
}

#[test]
fn a_keyless_group_is_drawn_as_is_on_a_search_draw() {
    // A group with no key has no target, cannot be opened by a click, and
    // draws as it is on a search draw: a match there would be one nothing
    // could reveal (`docs/tui.md`, "Search").
    let mut page = part();
    for (kind, action, payload) in [
        (
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": "hi"}]}]}),
        ),
        ("reasoning_started", Some("a_r"), json!({})),
        (
            "reasoning_completed",
            Some("a_r"),
            json!({"text": "hidden keyless words"}),
        ),
        (
            "tool_call_requested",
            Some("a_t"),
            json!({"name": "read", "arguments": {"path": "src/a.rs"}}),
        ),
        (
            "tool_call_completed",
            Some("a_t"),
            json!({"status": "completed", "content": [{"type": "text", "text": "ok"}]}),
        ),
        ("text_completed", Some("a_m"), json!({"text": "reply"})),
        ("turn_completed", None, json!({"outcome": "completed"})),
    ] {
        fold(&mut page, &envelope(kind, action, payload));
    }
    for card in &mut page.turns {
        for group in card.groups_mut() {
            group.key = None;
            group.open = false;
        }
    }
    let pages = Pages::new(80);
    let (shown, _, _, _) = pages.draw_data(0, &page, Draw::Shown);
    let (all, _, _, _) = pages.draw_data(0, &page, Draw::AllOpen);
    assert_eq!(drawn_texts(&shown), drawn_texts(&all));
    assert!(
        drawn_texts(&all)
            .iter()
            .all(|line| !line.contains("hidden keyless")),
        "a keyless ledger draws on a search draw"
    );
}

/// A `turn_started` line starting `text`.
fn begun(stream: &mut Stream, text: &str) {
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": text}]}]}),
    );
}

/// Durable filler lines that fold no row, to reach a page cut.
fn fill(stream: &mut Stream, n: usize) {
    for _ in 0..n {
        stream.durable("step_started", None, json!({}));
    }
}

/// A handoff, completed with an empty note.
fn handed_off(stream: &mut Stream) {
    stream.durable("handoff_started", None, json!({"trigger": "auto"}));
    stream.durable(
        "handoff_completed",
        None,
        json!({"outcome": "completed", "tokens_before": 1_000}),
    );
}

/// A completed turn.
fn completed(stream: &mut Stream) {
    stream.durable("turn_completed", None, json!({"outcome": "completed"}));
}

/// The session folded live at `width`.
fn pages_at(lines: &[Envelope], width: u16) -> Pages {
    let mut pages = Pages::new(width);
    for line in lines {
        pages.apply(line);
    }
    pages
}

/// Whether a shown row is a full-width card edge row.
fn shown_edge(shown: impl AsRef<str>, width: usize, edge: char) -> bool {
    let shown = shown.as_ref();
    !shown.is_empty() && shown.chars().count() == width && shown.chars().all(|ch| ch == edge)
}

/// Whether `line` is a full-width card edge row.
fn is_edge(line: &ratatui::text::Line<'static>, width: usize, edge: char) -> bool {
    shown_edge(line.to_string(), width, edge)
}

/// A resident page's rows as text.
fn cut_texts(pages: &Pages, at: usize) -> Vec<String> {
    pages
        .page_text(at)
        .map(|(rows, _)| texts(&rows))
        .unwrap_or_default()
}

#[test]
fn a_turn_cut_across_pages_draws_one_card() {
    // A running turn cut once: neither page draws the edge between them,
    // so the joined rows hold exactly one card edge pair for the turn
    // (`docs/tui.md`, "History and paging").
    let mut stream = Stream::new(false);
    begun(&mut stream, "go");
    stream.durable("step_started", None, json!({}));
    stream.text("a_m", "hello");
    fill(&mut stream, 62);
    stream.durable("step_started", None, json!({}));
    stream.text("a_m", "world");
    let lines = stream.lines;
    let mut pages = pages_at(&lines, 40);
    assert_eq!(pages.index().pages().len(), 2);
    let first = cut_texts(&pages, 0);
    let second = cut_texts(&pages, 1);
    assert!(!shown_edge(
        first.last().cloned().unwrap_or_default(),
        40,
        '▀'
    ));
    assert!(!shown_edge(
        second.first().cloned().unwrap_or_default(),
        40,
        '▄'
    ));
    let reference = whole(&pages, &lines);
    let got = joined(&mut pages, &lines);
    assert_eq!(differs(&got, &reference), None);
    assert_eq!(
        got.iter()
            .filter(|(line, _)| is_edge(line, 40, '▄'))
            .count(),
        1
    );
    assert_eq!(
        got.iter()
            .filter(|(line, _)| is_edge(line, 40, '▀'))
            .count(),
        1
    );
    drop_all(&mut pages);
    let got = joined(&mut pages, &lines);
    assert_eq!(differs(&got, &reference), None);
}

#[test]
fn only_the_cut_turn_loses_its_bottom_edge() {
    // A cut page holding a finished turn and then the running one: the
    // finished turn's card keeps its bottom edge
    // (`docs/tui.md`, "History and paging").
    let mut stream = Stream::new(false);
    begun(&mut stream, "one");
    stream.text("a_m", "done");
    completed(&mut stream);
    begun(&mut stream, "two");
    stream.text("a_m", "hello");
    fill(&mut stream, 62);
    stream.durable("step_started", None, json!({}));
    stream.text("a_m", "world");
    let lines = stream.lines;
    let mut pages = pages_at(&lines, 40);
    assert_eq!(pages.index().pages().len(), 2);
    let first = cut_texts(&pages, 0);
    let closing = first
        .iter()
        .position(|row| row.starts_with(" ▣ completed"))
        .expect("the finished turn's closing line");
    assert!(shown_edge(first[closing + 1].clone(), 40, '▀'));
    assert!(!shown_edge(
        first.last().cloned().unwrap_or_default(),
        40,
        '▀'
    ));
    let reference = whole(&pages, &lines);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
}

#[test]
fn a_new_turn_on_a_continuing_page_opens_its_card() {
    // The page after a cut holds the cut turn's end, then a second turn:
    // the second card opens with its edge
    // (`docs/tui.md`, "History and paging").
    let mut stream = Stream::new(false);
    begun(&mut stream, "go");
    stream.text("a_m", "hello");
    fill(&mut stream, 62);
    stream.durable("step_started", None, json!({}));
    stream.text("a_m", "world");
    begun(&mut stream, "next");
    stream.text("a_n", "ok");
    let lines = stream.lines;
    let mut pages = pages_at(&lines, 40);
    assert_eq!(pages.index().pages().len(), 2);
    let (rows, _) = pages.page_text(1).expect("the second page");
    let shown = texts(&rows);
    let bubble = shown
        .iter()
        .position(|row| row == " next ▐")
        .expect("the second turn's bubble");
    // The bubble, its bottom edge, its time row, a blank row, then the
    // card's top edge.
    assert!(is_edge(&rows[bubble + 4].0, 40, '▄'));
    let reference = whole(&pages, &lines);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
}

#[test]
fn a_cut_after_a_band_opens_the_next_page_with_an_edge() {
    // The handoff completes before the candidate step: the page before
    // the cut ends with the band's rows, and the next page opens with an
    // edge (`docs/tui.md`, "Handoff", "History and paging").
    let mut stream = Stream::new(false);
    begun(&mut stream, "go");
    stream.text("a_m", "before");
    handed_off(&mut stream);
    fill(&mut stream, 60);
    stream.durable("step_started", None, json!({}));
    stream.text("a_m", "after");
    completed(&mut stream);
    let lines = stream.lines;
    let mut pages = pages_at(&lines, 40);
    assert_eq!(pages.index().pages().len(), 2);
    let first = cut_texts(&pages, 0);
    // The page before the cut ends with the band's lower edge, its
    // content row above it.
    assert!(
        first
            .last()
            .is_some_and(|row| shown_edge(row.clone(), 40, '▀')),
        "{first:?}"
    );
    assert!(
        first
            .iter()
            .rev()
            .nth(1)
            .is_some_and(|row| row.contains('⇄')),
        "{first:?}"
    );
    let second = cut_texts(&pages, 1);
    assert!(shown_edge(
        second.first().cloned().unwrap_or_default(),
        40,
        '▄'
    ));
    let reference = whole(&pages, &lines);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
    drop_all(&mut pages);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
}

#[test]
fn a_cut_after_a_band_and_a_blank_reply_opens_the_next_page_with_an_edge() {
    // The band, then whitespace that renders no row, then the candidate
    // step and visible text: the page before the cut ends with the
    // band's rows, and the next page opens with an edge, because the
    // join is decided from the rows the pieces draw, never from entries
    // (`docs/tui.md`, "Handoff", "History and paging").
    let mut stream = Stream::new(false);
    begun(&mut stream, "go");
    stream.text("a_m", "before");
    handed_off(&mut stream);
    stream.text("a_m", "   ");
    fill(&mut stream, 60);
    stream.durable("step_started", None, json!({}));
    stream.text("a_m", "after");
    completed(&mut stream);
    let lines = stream.lines;
    let mut pages = pages_at(&lines, 40);
    assert_eq!(pages.index().pages().len(), 2);
    let first = cut_texts(&pages, 0);
    // The page before the cut ends with the band's lower edge, its
    // content row above it.
    assert!(
        first
            .last()
            .is_some_and(|row| shown_edge(row.clone(), 40, '▀')),
        "{first:?}"
    );
    assert!(
        first
            .iter()
            .rev()
            .nth(1)
            .is_some_and(|row| row.contains('⇄')),
        "{first:?}"
    );
    let second = cut_texts(&pages, 1);
    assert!(shown_edge(
        second.first().cloned().unwrap_or_default(),
        40,
        '▄'
    ));
    let reference = whole(&pages, &lines);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
    drop_all(&mut pages);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
}

#[test]
fn a_blank_reply_cut_then_the_turn_completes_joins_once_it_draws() {
    // A blank reply confirms the cut while the next page draws nothing
    // of the card; the turn's completion draws the closing line there,
    // and only then does the page before lose its edge
    // (`docs/tui.md`, "History and paging").
    let mut stream = Stream::new(false);
    begun(&mut stream, "go");
    stream.text("a_m", "visible");
    fill(&mut stream, 62);
    stream.durable("step_started", None, json!({}));
    stream.text("a_m", "   ");
    let cut = stream.lines.len();
    completed(&mut stream);
    let lines = stream.lines;
    // After the cut the page before still has its edge, and the next
    // page draws nothing of the turn's card.
    let mut pages = pages_at(&lines[..cut], 40);
    assert_eq!(pages.index().pages().len(), 2);
    let before = pages.index().pages()[0].rows;
    let first = cut_texts(&pages, 0);
    assert!(shown_edge(
        first.last().cloned().unwrap_or_default(),
        40,
        '▀'
    ));
    assert!(cut_texts(&pages, 1).is_empty());
    // The completion joins: the edge comes off the page before, and the
    // next page starts with the closing line and no edge.
    for line in &lines[cut..] {
        pages.apply(line);
    }
    let first = cut_texts(&pages, 0);
    assert!(!shown_edge(
        first.last().cloned().unwrap_or_default(),
        40,
        '▀'
    ));
    let second = cut_texts(&pages, 1);
    assert_eq!(second.first().cloned().unwrap_or_default(), "");
    assert!(
        second
            .get(1)
            .is_some_and(|row| row.starts_with(" ▣ completed")),
        "{second:?}"
    );
    assert_eq!(pages.index().pages()[0].rows, before - 1);
    let reference = whole(&pages, &lines);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
    drop_all(&mut pages);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
    // A new width keeps the join.
    pages.set_width(120);
    drop_all(&mut pages);
    let reference = whole(&pages, &lines);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
    // With the page before dropped, the count moves through the index.
    let mut pages = pages_at(&lines[..cut], 40);
    let before = pages.index().pages()[0].rows;
    pages.residency.drop_slot_for_test(0);
    for line in &lines[cut..] {
        pages.apply(line);
    }
    assert!(pages.part(0).is_none());
    assert_eq!(pages.index().pages()[0].rows, before - 1);
    let reference = whole(&pages, &lines);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
}

#[test]
fn a_cut_before_a_band_closes_the_piece_on_the_page_before() {
    // A handoff starts between the candidate step and the text that
    // confirms it: the page before keeps its edge, and the next page
    // starts with the band (`docs/tui.md`, "Handoff",
    // "History and paging").
    let mut stream = Stream::new(false);
    begun(&mut stream, "go");
    stream.text("a_m", "before");
    fill(&mut stream, 62);
    stream.durable("step_started", None, json!({}));
    stream.durable("handoff_started", None, json!({"trigger": "auto"}));
    stream.durable(
        "handoff_completed",
        None,
        json!({"outcome": "completed", "tokens_before": 1_000}),
    );
    stream.text("a_m", "after");
    completed(&mut stream);
    let lines = stream.lines;
    let mut pages = pages_at(&lines, 40);
    assert_eq!(pages.index().pages().len(), 2);
    let first = cut_texts(&pages, 0);
    assert!(shown_edge(
        first.last().cloned().unwrap_or_default(),
        40,
        '▀'
    ));
    let second = cut_texts(&pages, 1);
    // The next page opens with the band's upper edge, its content row
    // below it.
    assert!(
        second
            .first()
            .is_some_and(|row| shown_edge(row.clone(), 40, '▄')),
        "{second:?}"
    );
    assert!(
        second.get(1).is_some_and(|row| row.contains('⇄')),
        "{second:?}"
    );
    let reference = whole(&pages, &lines);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
    drop_all(&mut pages);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
}

#[test]
fn a_running_turn_with_only_its_bubble_does_not_join() {
    // The candidate step comes before any entry: the page before holds
    // only the bubble, and the next page opens its card with an edge
    // (`docs/tui.md`, "History and paging").
    let mut stream = Stream::new(false);
    begun(&mut stream, "go");
    fill(&mut stream, 64);
    stream.durable("step_started", None, json!({}));
    stream.text("a_m", "hello");
    let lines = stream.lines;
    let mut pages = pages_at(&lines, 40);
    assert_eq!(pages.index().pages().len(), 2);
    // The turn drew no piece on the page before the cut.
    let before = pages.residency.slots_for_test()[0]
        .as_ref()
        .and_then(|part| part.turns.last())
        .map(|card| {
            let mut out = Rows::default();
            let pieces = card.rows(
                40,
                &pages.zone,
                crate::surface::Edges::BOTH,
                &crate::image::Layout::default(),
                &mut out,
            );
            (pieces.first, pieces.last)
        });
    assert_eq!(before, Some((false, false)));
    let second = cut_texts(&pages, 1);
    assert!(shown_edge(
        second.first().cloned().unwrap_or_default(),
        40,
        '▄'
    ));
    let reference = whole(&pages, &lines);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
}

#[test]
fn a_refolded_page_keeps_its_joins() {
    // Dropping every page and reloading draws the same rows: the seeds
    // and the refold restore the joins (`docs/tui.md`,
    // "History and paging").
    let mut stream = Stream::new(false);
    begun(&mut stream, "go");
    stream.text("a_m", "hello");
    fill(&mut stream, 62);
    stream.durable("step_started", None, json!({}));
    stream.text("a_m", "world");
    let lines = stream.lines;
    let mut pages = pages_at(&lines, 40);
    let live = joined(&mut pages, &lines);
    assert!(pages.seeds[0].joins_next);
    assert!(pages.seeds[1].joins_previous);
    drop_all(&mut pages);
    let refolded = joined(&mut pages, &lines);
    assert_eq!(differs(&refolded, &live), None);
    assert!(pages.seeds[0].joins_next);
    assert!(pages.seeds[1].joins_previous);
}

#[test]
fn a_cut_at_turn_started_joins_nothing() {
    // A cut at a turn's start leaves both flags false: each card keeps
    // both edges (`docs/tui.md`, "History and paging").
    let mut stream = Stream::new(false);
    begun(&mut stream, "one");
    stream.text("a_m", "done");
    completed(&mut stream);
    fill(&mut stream, 61);
    begun(&mut stream, "two");
    stream.text("a_m", "hello");
    let lines = stream.lines;
    let mut pages = pages_at(&lines, 40);
    assert_eq!(pages.index().pages().len(), 2);
    assert!(!pages.seeds[0].joins_next);
    assert!(!pages.seeds[1].joins_previous);
    let first = cut_texts(&pages, 0);
    assert!(shown_edge(
        first.last().cloned().unwrap_or_default(),
        40,
        '▀'
    ));
    let second = cut_texts(&pages, 1);
    let bubble = second
        .iter()
        .position(|row| row == " two ▐")
        .expect("the second turn's bubble");
    assert!(shown_edge(&second[bubble + 4], 40, '▄'));
    let reference = whole(&pages, &lines);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
}

#[test]
fn joins_survive_a_new_width() {
    // Cut pages at 40, reloaded at 120, draw what one fold draws at 120
    // (`docs/tui.md`, "History and paging").
    let mut stream = Stream::new(false);
    begun(&mut stream, "go");
    stream.text("a_m", "hello");
    fill(&mut stream, 62);
    stream.durable("step_started", None, json!({}));
    stream.text("a_m", "world");
    let lines = stream.lines;
    let mut pages = pages_at(&lines, 40);
    assert_eq!(pages.index().pages().len(), 2);
    pages.set_width(120);
    drop_all(&mut pages);
    let reference = whole(&pages, &lines);
    assert_eq!(differs(&joined(&mut pages, &lines), &reference), None);
}

#[test]
fn shown_carries_the_spin_marks_of_its_lines() {
    let mut stream = Stream::new(false);
    stream.turn(0, 30);
    // A running tail: a turn, its step and its requested call, never
    // completed, so its group line moves on the open page.
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
    );
    stream.durable("step_started", None, json!({}));
    stream.durable("assistant_message_started", Some("a_mx"), json!({}));
    stream.durable(
        "tool_call_requested",
        Some("a_tx"),
        json!({"name": "read", "arguments": {"path": "src/x.rs"}}),
    );
    let mut pages = Pages::new(60);
    for line in &stream.lines {
        pages.apply(line);
    }
    assert!(pages.index().pages().len() > 1, "too few pages");
    let shown = pages.shown(0, usize::MAX);
    // The one moving line, by its index in the joined lines: finished
    // groups mark nothing.
    let moving = shown
        .lines
        .iter()
        .rposition(|(line, _, _)| line.to_string().starts_with(" ● Read 1 file"));
    assert_eq!(shown.spins, vec![(moving.unwrap_or(usize::MAX), 1)]);
}

#[test]
fn drawing_a_page_keeps_each_replys_render() {
    let mut stream = Stream::new(false);
    stream.turn(0, 1);
    let mut pages = Pages::new(60);
    for line in &stream.lines {
        pages.apply(line);
    }
    // Drawing the page renders its replies: reading them back must find
    // the stored cards' renders, not only their clones'.
    assert!(pages.page_text(0).is_some());
    let _ = pages.shown(0, 10);
    let part = pages.part(0).expect("page 0 is resident");
    let mut replies = part
        .turns
        .iter()
        .flat_map(|turn| &turn.entries)
        .filter_map(|entry| match entry {
            Entry::Reply { reply, .. } => Some(reply),
            Entry::Steer(_)
            | Entry::Group(_)
            | Entry::Aside(_)
            | Entry::Band(_)
            | Entry::Answers(_)
            | Entry::Message(_) => None,
        });
    let reply = replies.next().expect("the turn has a reply");
    assert_eq!(reply.cached_at(), Some(58));
}

/// One page-shaped group for the exact-work fixtures: nine full turns
/// with identical reply text, then one textless turn. The nine turns
/// hold 63 lines, so no step sees a full page; the textless turn's step
/// sets a candidate no text ever confirms, so the next group's first
/// turn cuts exactly one page. Ids are zero-padded to a fixed width:
/// unique across groups, so usage lines find their own turn, yet every
/// group draws the same text at the same widths however large the
/// session grows.
fn work_group(stream: &mut Stream, group: usize) {
    work_group_with(
        stream,
        group,
        "the session log holds every durable line in order ",
    );
}

/// One page-shaped group with `reply` as every full turn's text.
fn work_group_with(stream: &mut Stream, group: usize, reply: &str) {
    for turn in 0..9 {
        let message = format!("a_g{group:02}w{turn}");
        stream.durable(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("prompt {turn}")}]}]}),
        );
        stream.durable("step_started", None, json!({}));
        stream.durable("assistant_message_started", Some(&message), json!({}));
        stream.durable("text_completed", Some(&message), json!({"text": reply}));
        stream.durable(
            "assistant_message_completed",
            Some(&message),
            json!({"outcome": "completed"}),
        );
        stream.usage(&format!("g{group:02}w{turn}"), 100, json!({}));
        stream.durable("turn_completed", None, json!({"outcome": "completed"}));
    }
    // The textless turn: its step's candidate dies at its end, and its
    // lines bring the group to 69, so the next group cuts exactly once.
    let message = format!("a_g{group:02}q");
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "quiet"}]}]}),
    );
    stream.durable("step_started", None, json!({}));
    stream.durable("assistant_message_started", Some(&message), json!({}));
    stream.durable(
        "assistant_message_completed",
        Some(&message),
        json!({"outcome": "completed"}),
    );
    stream.usage(&format!("g{group:02}q"), 100, json!({}));
    stream.durable("turn_completed", None, json!({"outcome": "completed"}));
}

/// `groups` page-shaped groups folded at `width`, holding exactly one
/// page each: the next group's first turn cuts the last.
fn work_pages(groups: usize, width: u16) -> (Pages, Vec<Envelope>) {
    let mut stream = Stream::new(false);
    for group in 0..groups {
        work_group(&mut stream, group);
    }
    let mut pages = Pages::new(width);
    for line in &stream.lines {
        pages.apply(line);
    }
    assert_eq!(
        pages.index().pages().len(),
        groups,
        "{groups} groups cut {groups} pages"
    );
    (pages, stream.lines)
}

/// Drops the first half of the closed pages, so both fixtures keep the
/// same resident share: the open page and the later half stay.
fn drop_half(pages: &mut Pages) {
    pages.residency.drop_half_for_test();
}

/// Reloads every dropped page from `lines`, as the frame's loads do.
fn reload(pages: &mut Pages, lines: &[Envelope]) {
    for at in 0..pages.index().pages().len() {
        if pages.part(at).is_none() {
            let range = pages
                .index()
                .pages()
                .get(at)
                .map(|page| page.first_seq..=page.last_seq);
            if let Some(range) = range {
                pages.load(&SessionId(SESSION.to_owned()), &history(lines, &range));
            }
            assert!(pages.part(at).is_some(), "page {at} did not load");
        }
    }
}

#[test]
fn page_shaped_groups_cut_exactly_one_page_each() {
    for groups in [8, 32] {
        let (pages, _) = work_pages(groups, 80);
        assert_eq!(pages.page_count(), groups);
        // Identical groups draw identical rows on every page.
        let mut rows = Vec::new();
        for at in 0..groups {
            let part = pages.part(at).expect("every page is resident");
            let mut out = Rows::default();
            pages.draw(at, part, &mut out);
            rows.push(out.into_parts().0.len());
        }
        assert!(
            rows.iter().all(|count| *count == rows[0]),
            "every page draws {rows:?}"
        );
    }
}

/// The recount and reload work, by phase: re-counting the resident pages
/// at the new width, then reloading the dropped ones.
fn recount_work(pages: &mut Pages, lines: &[Envelope], width: u16) -> [crate::work::Work; 2] {
    crate::work::take();
    pages.set_width(width);
    let recount = crate::work::take();
    reload(pages, lines);
    let reloaded = crate::work::take();
    [recount, reloaded]
}

#[test]
fn a_recount_at_a_new_width_costs_pages_not_the_session() {
    let (mut small, small_lines) = work_pages(8, 80);
    let (mut large, large_lines) = work_pages(32, 80);
    // The same resident share: the open page and the later half stay.
    drop_half(&mut small);
    drop_half(&mut large);
    assert_eq!(small.resident(), 5);
    assert_eq!(large.resident(), 17);
    let [small_recount, small_reload] = recount_work(&mut small, &small_lines, 100);
    let [large_recount, large_reload] = recount_work(&mut large, &large_lines, 100);
    // Every counter grows with the pages, never the session: four times
    // the pages, at most four times the work plus the open page and the
    // frame's bookkeeping.
    let fields = [
        (
            "reply_renders",
            small_recount.reply_renders + small_reload.reply_renders,
            large_recount.reply_renders + large_reload.reply_renders,
        ),
        (
            "turn_rows",
            small_recount.turn_rows + small_reload.turn_rows,
            large_recount.turn_rows + large_reload.turn_rows,
        ),
        (
            "page_counts",
            small_recount.page_counts + small_reload.page_counts,
            large_recount.page_counts + large_reload.page_counts,
        ),
        (
            "index_pages",
            small_recount.index_pages + small_reload.index_pages,
            large_recount.index_pages + large_reload.index_pages,
        ),
        (
            "usage_summaries",
            small_recount.usage_summaries + small_reload.usage_summaries,
            large_recount.usage_summaries + large_reload.usage_summaries,
        ),
        (
            "grapheme_widths",
            small_recount.grapheme_widths + small_reload.grapheme_widths,
            large_recount.grapheme_widths + large_reload.grapheme_widths,
        ),
        (
            "search_pages",
            small_recount.search_pages + small_reload.search_pages,
            large_recount.search_pages + large_reload.search_pages,
        ),
        (
            "search_order_pages",
            small_recount.search_order_pages + small_reload.search_order_pages,
            large_recount.search_order_pages + large_reload.search_order_pages,
        ),
        (
            "shell_visits",
            small_recount.shell_visits + small_reload.shell_visits,
            large_recount.shell_visits + large_reload.shell_visits,
        ),
    ];
    for (field, small, large) in fields {
        assert!(
            large <= 4 * small + 64,
            "{field}: {large} is more than four times {small} plus the allowance"
        );
    }
    // The small fixture's own counts pin the seams: resident pages count
    // once at the new width and dropped ones once on reload, and neither
    // phase walks the index at all: row queries wait for the next total
    // or window instead of rebuilding prefixes between every page, and
    // no search ran at all.
    assert_eq!(small_recount.page_counts, 5, "resident pages recount");
    assert_eq!(small_reload.page_counts, 3, "dropped pages reload");
    assert_eq!(
        small_recount.index_pages, 0,
        "no index walk while recounting"
    );
    assert_eq!(small_reload.index_pages, 0, "no index walk while reloading");
    assert_eq!(
        large_recount.index_pages, 0,
        "no index walk while recounting"
    );
    assert_eq!(large_reload.index_pages, 0, "no index walk while reloading");
    assert_eq!(small_recount.search_pages, 0);
    assert_eq!(small_reload.search_pages, 0);
    assert_eq!(small_recount.shell_visits, 0);
    assert_eq!(small_reload.shell_visits, 0);
    assert_eq!(small_recount.usage_summaries, 0);
    assert_eq!(small_reload.usage_summaries, 0);
    // All rows still draw where they did: the recount moved no row.
    for (pages, lines) in [(&small, &small_lines), (&large, &large_lines)] {
        let mut total = 0usize;
        for at in 0..pages.index().pages().len() {
            let part = pages.part(at).expect("every page reloaded");
            let mut out = Rows::default();
            pages.draw(at, part, &mut out);
            total += out.into_parts().0.len();
        }
        assert_eq!(total, pages.index().total(), "rows add to the total");
        // Each group folds 69 durable lines and cuts exactly one page.
        assert_eq!(lines.len(), pages.index().pages().len() * 69);
    }
}

/// The per-page proof for the scale-16 ceiling (#2187): identical pages
/// do identical work, so the re-count and reload counters per page agree
/// within 10% between the 8-page and 32-page sessions. The comparison is
/// synchronous counter work: it reads no clock and waits on nothing, so
/// no injected clock or deadline applies.
#[test]
fn recount_work_per_page_agrees_within_ten_percent_across_scales() {
    let (mut small, small_lines) = work_pages(8, 80);
    let (mut large, large_lines) = work_pages(32, 80);
    drop_half(&mut small);
    drop_half(&mut large);
    let small_pages = small.index().pages().len();
    let large_pages = large.index().pages().len();
    assert_eq!(small_pages, 8);
    assert_eq!(large_pages, 32);
    let [small_recount, small_reload] = recount_work(&mut small, &small_lines, 100);
    let [large_recount, large_reload] = recount_work(&mut large, &large_lines, 100);
    let fields = [
        (
            "reply_renders",
            small_recount.reply_renders + small_reload.reply_renders,
            large_recount.reply_renders + large_reload.reply_renders,
        ),
        (
            "turn_rows",
            small_recount.turn_rows + small_reload.turn_rows,
            large_recount.turn_rows + large_reload.turn_rows,
        ),
        (
            "page_counts",
            small_recount.page_counts + small_reload.page_counts,
            large_recount.page_counts + large_reload.page_counts,
        ),
        (
            "index_pages",
            small_recount.index_pages + small_reload.index_pages,
            large_recount.index_pages + large_reload.index_pages,
        ),
        (
            "usage_summaries",
            small_recount.usage_summaries + small_reload.usage_summaries,
            large_recount.usage_summaries + large_reload.usage_summaries,
        ),
        (
            "grapheme_widths",
            small_recount.grapheme_widths + small_reload.grapheme_widths,
            large_recount.grapheme_widths + large_reload.grapheme_widths,
        ),
        (
            "search_pages",
            small_recount.search_pages + small_reload.search_pages,
            large_recount.search_pages + large_reload.search_pages,
        ),
        (
            "search_order_pages",
            small_recount.search_order_pages + small_reload.search_order_pages,
            large_recount.search_order_pages + large_reload.search_order_pages,
        ),
        (
            "shell_visits",
            small_recount.shell_visits + small_reload.shell_visits,
            large_recount.shell_visits + large_reload.shell_visits,
        ),
    ];
    for (field, small, large) in fields {
        // Cross-multiplied per-page comparison: `large / large_pages`
        // against `small / small_pages`, within 10%, without dividing.
        let small_scaled = small * large_pages;
        let large_scaled = large * small_pages;
        if small == 0 {
            assert_eq!(
                large, 0,
                "{field}: no work at 8 pages must stay no work at 32"
            );
        } else {
            let diff = large_scaled.abs_diff(small_scaled);
            assert!(
                diff * 10 <= small_scaled,
                "{field}: per-page {large_scaled} over {large_pages} pages differs from {small_scaled} over {small_pages} by more than 10%"
            );
        }
    }
}

/// Two pages: plain prose, then a reply holding a Markdown table with
/// cells long enough to lay out differently at 80 and 100 columns.
fn table_pages(width: u16) -> (Pages, Vec<Envelope>) {
    let table = "| left hand side | right hand side |\n|---|---|\n| a cell with quite a lot of words inside it | another cell with quite a lot of words inside it |\n";
    let mut stream = Stream::new(false);
    work_group(&mut stream, 0);
    work_group_with(&mut stream, 1, table);
    let mut pages = Pages::new(width);
    for line in &stream.lines {
        pages.apply(line);
    }
    assert_eq!(pages.index().pages().len(), 2, "two groups cut two pages");
    (pages, stream.lines)
}

#[test]
fn a_width_change_moves_only_table_page_revisions() {
    let (mut pages, _) = table_pages(80);
    // Both pages counted, so both classified: prose is width-stable.
    let plain = pages.index().revision(0);
    let tabled = pages.index().revision(1);
    pages.set_width(100);
    assert_eq!(
        pages.index().revision(0),
        plain,
        "a non-table page keeps its revision across a width change"
    );
    assert!(
        pages.index().revision(1) > tabled,
        "a table page scans again at the new width"
    );
    // The same width again settles nothing: no page rescans twice for
    // one width change.
    let plain = pages.index().revision(0);
    let tabled = pages.index().revision(1);
    pages.set_width(100);
    assert_eq!(pages.index().revision(0), plain);
    assert_eq!(pages.index().revision(1), tabled);
    // Back at the first width the table page moves once more, and the
    // prose page still holds.
    pages.set_width(80);
    assert_eq!(pages.index().revision(0), plain);
    assert!(
        pages.index().revision(1) > tabled,
        "returning to a width rescans the table page once"
    );
    // A tiny width recounts every row yet moves the same revisions:
    // geometry never stands in for text.
    let plain = pages.index().revision(0);
    let tabled = pages.index().revision(1);
    pages.set_width(10);
    assert!(pages.index().pages()[0].rows > 0);
    assert_eq!(pages.index().revision(0), plain);
    assert!(pages.index().revision(1) > tabled);
    let total = pages.index().total();
    let mut drawn = 0usize;
    for at in 0..pages.index().pages().len() {
        let part = pages.part(at).expect("every page is resident");
        let mut out = Rows::default();
        pages.draw(at, part, &mut out);
        drawn += out.into_parts().0.len();
    }
    assert_eq!(drawn, total);
}

#[test]
fn an_unclassified_page_rescans_once_then_holds() {
    // A fresh page was never counted, so its sensitivity is unknown:
    // the first width change rescans it rather than trusting it.
    let mut pages = Pages::new(80);
    assert_eq!(pages.index().revision(0), 0);
    pages.set_width(100);
    assert_eq!(pages.index().revision(0), 1);
    // Counting classifies its empty cards as width-stable, so the next
    // width change leaves it alone.
    pages.set_width(80);
    assert_eq!(pages.index().revision(0), 1);
}

#[test]
fn a_batch_held_line_moves_the_revision_before_the_count() {
    // One short turn on one page, so no step sees a full page and no
    // line cuts while the batch folds.
    let mut stream = Stream::new(false);
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "prompt"}]}]}),
    );
    stream.durable("step_started", None, json!({}));
    stream.durable("assistant_message_started", Some("a_h"), json!({}));
    stream.durable("text_completed", Some("a_h"), json!({"text": "first"}));
    let mut pages = Pages::new(80);
    for line in &stream.lines {
        pages.apply(line);
    }
    let open = pages.residency.len();
    let revision = pages.index().revision(open);
    let recounts = pages.recounts;
    // The batch's lines change the running turn's cards.
    let mut held = Stream::new(false);
    held.durable("step_started", None, json!({}));
    held.durable("assistant_message_started", Some("a_h2"), json!({}));
    held.durable("text_completed", Some("a_h2"), json!({"text": "held line"}));
    pages.hold();
    let mut changed = false;
    for line in &held.lines {
        changed |= pages.apply(line).changed;
    }
    assert!(changed, "the held batch changes a card");
    // The text changed under the hold: the revision moved at once, while
    // the recount waits for the batch's end.
    assert!(pages.index().revision(open) > revision);
    assert_eq!(pages.recounts, recounts);
    pages.flush();
    assert!(pages.recounts > recounts);
}

#[test]
fn rejoining_an_edge_moves_no_search_revision() {
    // The blank-reply cut: the next page draws nothing of the card, so
    // the page before keeps its edge until the turn's completion draws
    // the closing line there and joins the pages.
    let mut stream = Stream::new(false);
    begun(&mut stream, "go");
    stream.text("a_m", "visible");
    fill(&mut stream, 62);
    stream.durable("step_started", None, json!({}));
    stream.text("a_m", "   ");
    let cut = stream.lines.len();
    completed(&mut stream);
    let lines = stream.lines;
    let mut pages = pages_at(&lines[..cut], 40);
    assert_eq!(pages.index().pages().len(), 2);
    let rows = pages.index().pages()[0].rows;
    let revision = pages.index().revision(0);
    let open_revision = pages.index().revision(1);
    for line in &lines[cut..] {
        pages.apply(line);
    }
    // The edge came off, so the row count moved ...
    assert_eq!(pages.index().pages()[0].rows, rows - 1);
    // ... but only an edge did: the page before keeps its revision,
    // while the open page's new closing line moves its own.
    assert_eq!(pages.index().revision(0), revision);
    assert!(pages.index().revision(1) > open_revision);
}

#[test]
fn a_reply_gaining_a_table_reclassifies_its_page() {
    // One page with a plain reply: counted, so classified width-stable.
    let mut stream = Stream::new(true);
    begun(&mut stream, "go");
    stream.durable("assistant_message_started", Some("a_m"), json!({}));
    stream.text("a_m", "plain words here");
    let mut pages = Pages::new(80);
    for line in &stream.lines {
        pages.apply(line);
    }
    let revision = pages.index().revision(0);
    pages.set_width(100);
    assert_eq!(
        pages.index().revision(0),
        revision,
        "a table-free page keeps its revision"
    );
    // A delta streams a table onto the reply: the next count flips the
    // page to table-sensitive, so the width back rescans it.
    let mut table = Stream::new(true);
    table.durable(
        "assistant_message_delta",
        Some("a_m"),
        json!({"text": "| a | b |\n|---|---|\n| c | d |\n"}),
    );
    table.text("a_m", "plain words here\n| a | b |\n|---|---|\n| c | d |\n");
    for line in &table.lines {
        pages.apply(line);
    }
    pages.set_width(80);
    assert!(
        pages.index().revision(0) > revision,
        "a page that gained a table rescans at the new width"
    );
}

/// `groups` page-shaped groups with shell output after every `every`th
/// group, placed on the group it ran in.
fn work_pages_shells(groups: usize, width: u16, every: usize) -> (Pages, Vec<Envelope>) {
    let mut stream = Stream::new(false);
    for group in 0..groups {
        work_group(&mut stream, group);
    }
    let mut pages = Pages::new(width);
    let mut at = 0usize;
    for line in &stream.lines {
        pages.apply(line);
        at += 1;
        // After a group's last line the open page is the group's own:
        // the shell lands on it. A group folds 69 lines.
        if at.is_multiple_of(69) {
            let group = at / 69 - 1;
            if group.is_multiple_of(every) {
                assert!(pages.add_shell(Some(format!("shell {group:02}"))));
            }
        }
    }
    assert_eq!(
        pages.index().pages().len(),
        groups,
        "{groups} groups cut {groups} pages"
    );
    (pages, stream.lines)
}

#[test]
fn shells_on_many_pages_place_locally() {
    let (mut small, small_lines) = work_pages_shells(8, 80, 2);
    let (mut large, large_lines) = work_pages_shells(32, 80, 2);
    // Every shell draws on its own page, after its own turns.
    for (pages, groups) in [(&small, 8), (&large, 32)] {
        for group in (0..groups).step_by(2) {
            let texts = cut_texts(pages, group);
            assert!(
                texts.iter().any(|row| row == &format!("shell {group:02}")),
                "page {group} draws its shell: {texts:?}"
            );
        }
    }
    drop_half(&mut small);
    drop_half(&mut large);
    let [small_recount, small_reload] = recount_work(&mut small, &small_lines, 100);
    let [large_recount, large_reload] = recount_work(&mut large, &large_lines, 100);
    let small_shells = small_recount.shell_visits + small_reload.shell_visits;
    let large_shells = large_recount.shell_visits + large_reload.shell_visits;
    // Four shells on eight pages, sixteen on thirty-two: each shell's
    // page counts once, resident at the new width or dropped on reload,
    // so each shell is visited exactly once, never the session's shells.
    assert_eq!(small_shells, 4, "one visit per shell");
    assert!(
        large_shells <= 4 * small_shells + 64,
        "{large_shells} is more than four times {small_shells} plus the allowance"
    );
    // And the whole recount still grows with the pages.
    assert!(
        large_recount.page_counts + large_reload.page_counts
            <= 4 * (small_recount.page_counts + small_reload.page_counts) + 64
    );
}

#[test]
fn add_shell_without_output_reports_nothing_added() {
    let mut pages = Pages::new(80);
    // No shell ran, so no page changed: the `None` path reports false
    // while a real shell still reports true.
    assert!(!pages.add_shell(None));
    assert!(pages.add_shell(Some("shell 00".to_owned())));
}

#[test]
fn a_line_that_folds_nothing_moves_no_search_revision() {
    // A `step_started` with no running turn folds nothing: the open
    // page's search revision stays, so a search never rescans for it.
    // This pins the `changed &&` side of the invalidation guard: with
    // `||` every unchanged line would invalidate the page it lands on.
    let mut pages = Pages::new(80);
    let applied = pages.apply(&envelope("step_started", None, json!({})));
    assert!(!applied.changed, "nothing folds without a running turn");
    assert_eq!(pages.index().revision(0), 0);
    assert_eq!(pages.index().clock(), 0);
}

#[test]
fn a_never_counted_page_rescans_after_a_width_change() {
    // A page the width never counted has no table answer yet: the search
    // treats it as table-sensitive, so a width change moves its revision.
    // This pins the unknown-conservative default: answering false would
    // reuse a table layout scanned at another width.
    let mut pages = Pages::new(80);
    assert!(pages.page_table(0), "unknown pages read sensitive");
    pages.set_width(100);
    assert_eq!(
        pages.index().revision(0),
        1,
        "the unknown page invalidates at the new width"
    );
}

#[test]
fn trim_drops_only_pages_outside_the_window_and_keeps_pins() {
    // Thirty-two pages with the first half dropped directly (as tests
    // do) and reloaded through keeps: trimming at the bottom keeps
    // exactly the window's pages, and a pinned page outside it stays.
    // This pins the resident tracking trim drops from: a missed keep
    // would leave a reloaded page undroppable, and a missed drop would
    // keep the session resident.
    let (mut pages, lines) = work_pages(32, 80);
    drop_half(&mut pages);
    for at in 0..pages.index().pages().len() {
        if pages.part(at).is_none() {
            let range = pages
                .index()
                .pages()
                .get(at)
                .map(|page| page.first_seq..=page.last_seq);
            if let Some(range) = range {
                pages.load(&SessionId(SESSION.to_owned()), &history(&lines, &range));
            }
        }
    }
    let total = pages.index().total();
    let height = 24usize;
    let top = total.saturating_sub(height);
    pages.want(0);
    pages.trim(top, height);
    let window = pages.index().window(top, height);
    assert!(
        !window.contains(&0),
        "the pinned page sits outside the window"
    );
    for at in 0..pages.index().pages().len() {
        let kept = pages.part(at).is_some();
        if at == 0 {
            assert!(kept, "a pinned page stays resident");
        } else {
            assert_eq!(kept, window.contains(&at), "page {at} trims to the window");
        }
    }
    pages.unwant(0);
}

/// One turn per call: a prompt, one step and one requested call.
fn call_stream(calls: &[&str]) -> Vec<Envelope> {
    let mut stream = Stream::new(false);
    for (turn, call) in calls.iter().enumerate() {
        stream.durable(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("prompt {turn}")}]}]}),
        );
        stream.durable("step_started", None, json!({}));
        stream.durable("assistant_message_started", Some("a_m"), json!({}));
        stream.request("a_m", 0, call, "read", "src/lib.rs");
        stream.durable(
            "assistant_message_completed",
            Some("a_m"),
            json!({"outcome": "completed"}),
        );
        stream.durable("turn_completed", None, json!({"outcome": "completed"}));
    }
    stream.lines
}

/// Whether each resident call for `action` draws open, in fold order.
fn call_states(pages: &Pages, action: &str) -> Vec<bool> {
    pages
        .residency
        .resident()
        .map(|(_, part)| part)
        .chain(std::iter::once(&pages.open))
        .flat_map(|part| part.turns.iter())
        .flat_map(Turn::groups)
        .flat_map(|group| group.sections.iter())
        .flat_map(|section| section.calls.iter())
        .filter(|call| call.action == action)
        .map(|call| call.open)
        .collect()
}

#[test]
fn a_fresh_pages_keeps_its_settings_and_nothing_else() {
    let mut pages = Pages::new(37);
    for line in &call_stream(&["a_t"]) {
        pages.apply(line);
    }
    pages.zone = new_york();
    pages.toggle_ledgers();
    pages.open(&Target::Call(target_id("a_t")));
    pages.hold();
    let fresh = pages.fresh();
    assert_eq!(fresh.wrap_width(), 37);
    assert_eq!(fresh.zone, new_york());
    assert!(fresh.fold.ledgers);
    assert_eq!(fresh.images.sizing, pages.images.sizing);
    assert_eq!(fresh.index().total(), Pages::new(37).index().total());
    assert_eq!(fresh.resident(), Pages::new(37).resident());
    assert!(call_states(&fresh, "a_t").is_empty());
    assert!(fresh.overrides.is_empty());
    assert!(fresh.shells.is_empty());
    assert!(fresh.summaries.is_empty());
    assert!(!fresh.holding());
}

#[test]
fn opens_in_the_preview_carry_to_the_fold_by_call() {
    let mut preview = Pages::new(80);
    for line in &call_stream(&["a_dup"]) {
        preview.apply(line);
    }
    preview.open(&Target::Call(target_id("a_dup")));
    preview
        .overrides
        .insert(Target::Call(target_id("a_ghost")), true);
    // The fold asks for the same call twice: the same card id names two
    // calls, and only the first is the one the preview opened.
    let mut fold = preview.fresh();
    for line in &call_stream(&["a_dup", "a_dup"]) {
        fold.apply(line);
    }
    preview.carry_opens(&mut fold);
    assert_eq!(call_states(&fold, "a_dup"), vec![true, false]);
    assert_eq!(
        fold.overrides.get(&Target::Call(target_id("a_dup"))),
        Some(&true)
    );
    assert!(
        !fold
            .overrides
            .contains_key(&Target::Call(target_id("a_ghost"))),
        "a target the fold lacks is dropped"
    );
}

#[test]
fn a_ctrl_o_in_the_preview_carries_to_the_fold() {
    let lines = call_stream(&["a_t"]);
    let mut preview = Pages::new(80);
    for line in &lines {
        preview.apply(line);
    }
    // The fold is made before Ctrl+O: carrying the overrides only would
    // leave it shut.
    let mut fold = preview.fresh();
    for line in &lines {
        fold.apply(line);
    }
    preview.toggle_ledgers();
    preview.carry_opens(&mut fold);
    assert!(fold.fold.ledgers);
    let ledgers: Vec<bool> = fold
        .residency
        .resident()
        .map(|(_, part)| part)
        .chain(std::iter::once(&fold.open))
        .flat_map(|part| part.turns.iter())
        .flat_map(Turn::groups)
        .filter(|group| group.has_ledger())
        .map(|group| group.open)
        .collect();
    assert!(!ledgers.is_empty());
    assert!(ledgers.iter().all(|open| *open));
}

/// A row's `seq` and its offset among that line's rows find the row
/// again, on the live fold and on every page folded again from its
/// lines; an offset past the piece stays on the piece's last row, and
/// a `seq` no page holds, or a row past the end, finds none.
#[test]
fn a_rows_seq_and_offset_find_the_row_again() {
    let lines = session(2, false);
    let mut pages = Pages::new(60);
    for line in &lines {
        pages.apply(line);
    }
    assert!(pages.index().pages().len() > 2, "too few pages");
    let total = pages.index().total();
    let check = |pages: &Pages| {
        for row in 0..total {
            let (session, seq, offset) = pages
                .row_seq(row)
                .unwrap_or_else(|| panic!("row {row} has a seq"));
            assert_eq!(pages.seq_row(&session, seq, offset), Some(row), "row {row}");
            let last = pages
                .seq_row(&session, seq, usize::MAX)
                .unwrap_or_else(|| panic!("row {row}'s piece has a last row"));
            assert!(last >= row, "row {row}");
            assert_eq!(
                pages.row_seq(last).map(|(_, seq, _)| seq),
                Some(seq),
                "row {row}"
            );
            assert_ne!(
                pages.row_seq(last.saturating_add(1)).map(|(_, at, _)| at),
                Some(seq),
                "row {row}'s piece ends at {last}"
            );
        }
    };
    check(&pages);
    drop_all(&mut pages);
    joined(&mut pages, &lines);
    assert_eq!(pages.index().total(), total);
    check(&pages);
    let end = lines.iter().filter_map(|line| line.seq).max();
    let past = end.map(|seq| Seq(seq.0.saturating_add(1)));
    let session = SessionId(SESSION.to_owned());
    assert_eq!(past.and_then(|seq| pages.seq_row(&session, seq, 0)), None);
    assert_eq!(pages.row_seq(total), None);
}

/// Two steps with thinking and a reply each, folded into fresh pages:
/// two groups keyed by their thought's action, two thoughts and two
/// replies, in fold order.
fn small() -> Pages {
    let mut stream = Stream::new(false);
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "look at the code"}]}]}),
    );
    for (thought, message, reply) in [
        ("a_r0", "a_m0", "reply zero here"),
        ("a_r1", "a_m1", "reply one here"),
    ] {
        stream.durable("step_started", None, json!({}));
        stream.durable("assistant_message_started", Some(message), json!({}));
        stream.think(thought, "weigh it");
        stream.text(message, reply);
        stream.durable(
            "assistant_message_completed",
            Some(message),
            json!({"outcome": "completed"}),
        );
    }
    stream.durable("turn_completed", None, json!({"outcome": "completed"}));
    let mut pages = Pages::new(80);
    for line in &stream.lines {
        pages.apply(line);
    }
    pages
}

/// Groups resolve by the action that keyed them, both ways: the
/// target a reload carries (`same_target`) is the key out and the
/// fold's id back in.
#[test]
fn group_targets_resolve_by_action() {
    let pages = small();
    for key in ["a_r0", "a_r1"] {
        let id = target_id(key);
        assert_eq!(
            pages.group_key(id),
            Some(key.to_owned()),
            "{key} names its group"
        );
        assert_eq!(pages.find_group(key), Some(id), "{key} resolves back");
    }
    assert_eq!(pages.group_key(target_id("a_missing")), None, "no group");
    assert_eq!(pages.find_group("a_missing"), None, "no group");
}

/// Thoughts resolve by action both ways, and the fold lists every
/// resident thought in fold order.
#[test]
fn thought_targets_resolve_by_action() {
    let pages = small();
    assert_eq!(
        pages.thoughts().collect::<Vec<_>>(),
        vec![
            (target_id("a_r0"), "a_r0".to_owned()),
            (target_id("a_r1"), "a_r1".to_owned()),
        ],
        "every resident thought in fold order"
    );
    for action in ["a_r0", "a_r1"] {
        assert_eq!(
            pages.thought_action(target_id(action)),
            Some(action.to_owned()),
            "{action} names its thought"
        );
        assert_eq!(
            pages.find_thought(action),
            Some(target_id(action)),
            "{action} resolves back"
        );
    }
    assert_eq!(
        pages.thought_action(target_id("a_missing")),
        None,
        "no thought"
    );
    assert_eq!(pages.find_thought("a_missing"), None, "no thought");
}

/// Copy targets resolve by reply ordinal both ways: the ordinal out
/// and the reply at an ordinal back in, over every resident reply.
#[test]
fn copy_targets_resolve_by_reply_ordinal() {
    let pages = small();
    let replies: Vec<usize> = pages.replies().collect();
    assert_eq!(replies.len(), 2, "two resident replies");
    let (first, second) = (replies[0], replies[1]);
    assert_ne!(first, second, "each reply has its own id");
    assert_eq!(pages.reply_ordinal(first), Some(0));
    assert_eq!(pages.reply_ordinal(second), Some(1));
    assert_eq!(pages.reply_ordinal(usize::MAX), None, "no reply");
    assert_eq!(pages.reply_at(0), Some(first));
    assert_eq!(pages.reply_at(1), Some(second));
    assert_eq!(pages.reply_at(2), None, "two replies only");
}

/// Image targets resolve by path: the folded image's id names its
/// path back, and unknown ids name nothing.
#[test]
fn image_targets_resolve_by_path() {
    let mut stream = Stream::new(false);
    stream.durable(
        "turn_started",
        None,
        json!({"input": [{"type": "message", "source": "driver",
        "content": [
            {"type": "text", "text": "look"},
            {"type": "image", "path": "artifacts/shot.png",
                "mime_type": "image/png", "width": 1280, "height": 800},
        ]}]}),
    );
    let mut pages = Pages::new(80);
    for line in &stream.lines {
        pages.apply(line);
    }
    assert_eq!(
        pages.image(1).map(|part| part.path.as_str()),
        Some("artifacts/shot.png")
    );
    assert_eq!(pages.image(0), None, "no image at zero");
    assert_eq!(pages.image(2), None, "one image only");
}

/// A page with rows but no line holds no seq: the row's piece starts
/// at or above it only once a line drew it.
#[test]
fn row_seq_needs_a_line_on_the_page() {
    let mut pages = Pages::new(80);
    pages.index.set_rows(0, 3);
    assert_eq!(pages.row_seq(0), None, "a page with no line holds no seq");
    assert_eq!(pages.row_seq(2), None, "a page with no line holds no seq");
    assert_eq!(pages.row_seq(3), None, "past the last row");
    let pages = small();
    let (session, seq, offset) = pages.row_seq(0).expect("the first row has a seq");
    assert_eq!(offset, 0);
    assert_eq!(
        pages.seq_row(&session, seq, offset),
        Some(0),
        "the first row round-trips"
    );
}

/// A seq no piece starts at lands on the first piece after it: the
/// exact arm never sees it, so only `>` (not `<`, which lands on the
/// first piece, or `==`, which lands on the page's first row) picks
/// the next piece.
#[test]
fn seq_row_skips_seqs_without_a_piece() {
    let lines = session(2, false);
    let mut pages = Pages::new(60);
    for line in &lines {
        pages.apply(line);
    }
    let mut gap = None;
    for (at, runs) in pages.row_seqs.iter().enumerate() {
        for pair in runs.windows(2) {
            let ((_, from), (second_start, next)) = (pair[0], pair[1]);
            // The next piece must start below the page's first row:
            // both mutants land on the page's first row, so a piece
            // starting there would not tell them apart.
            if next.0 > from.0.saturating_add(1) && second_start > 0 {
                gap = Some((at, Seq(from.0.saturating_add(1)), second_start));
            }
        }
    }
    let (at, between, second_start) = gap.expect("a seq between two pieces");
    // The gap sits on the single test session's page.
    let session = SessionId(SESSION.to_owned());
    assert_eq!(
        pages.seq_row(&session, between, 0),
        Some(pages.index.start(at).saturating_add(second_start)),
        "a seq without a piece lands on the next one"
    );
}

/// A landing anchor names its segment: a `seq` held by both the
/// ancestor and the own run resolves to the anchor session's row,
/// never the earliest segment's.
#[test]
fn a_landing_anchor_resolves_by_session_and_seq() {
    let (_ancestor, log) = chain();
    let mut pages = Pages::new(80);
    for line in &log {
        pages.apply(line);
    }
    let own = SessionId(SESSION.to_owned());
    let seam = pages
        .index()
        .pages()
        .iter()
        .position(|page| page.session == own)
        .expect("the own run");
    // The seam's first row: its `seq` also runs in the ancestor,
    // since both segments number from zero.
    let row = pages.index().start(seam);
    let (session, seq, offset) = pages.row_seq(row).expect("the seam row has a seq");
    assert_eq!(session, own, "the seam row is the own run's");
    assert!(
        pages
            .index()
            .page_of(&SessionId(ANCESTOR.to_owned()), seq)
            .is_some(),
        "the ancestor shares the seq"
    );
    pages.hold_anchor(&session, seq, offset);
    assert_eq!(
        pages.anchored_row(),
        Some(row),
        "the anchor keeps its session"
    );
}

/// A dropped page still waits while it is neither counted nor failed:
/// the pass stays live only while one is owed.
#[test]
fn stale_owed_names_an_uncounted_dropped_page() {
    assert!(
        !Pages::new(80).stale_owed(),
        "an empty conversation owes nothing"
    );
    let mut pages = Pages::new(80);
    pages.residency.stale_mut().insert(0);
    assert!(pages.stale_owed(), "a dropped page waits to be counted");
    pages.residency.failed_mut().insert(0);
    assert!(!pages.stale_owed(), "a failed page is owed nothing");
}

/// The anchor lets go once no stale page is owed, and not before: a
/// single call still returns the row either way.
#[test]
fn anchored_row_clears_only_once_no_page_is_stale() {
    let lines = session(1, false);
    let mut pages = Pages::new(80);
    for line in &lines {
        pages.apply(line);
    }
    let (session, seq, offset) = pages.row_seq(0).expect("the first row has a seq");
    pages.hold_anchor(&session, seq, offset);
    assert_eq!(
        pages.anchored_row(),
        Some(0),
        "a single call returns the row"
    );
    assert_eq!(
        pages.anchored_row(),
        None,
        "nothing stale is owed, so the anchor lets go"
    );
    pages.hold_anchor(&session, seq, offset);
    pages.residency.stale_mut().insert(0);
    assert_eq!(
        pages.anchored_row(),
        Some(0),
        "a single call returns the row while a page is stale"
    );
    assert_eq!(
        pages.anchored_row(),
        Some(0),
        "an owed stale page keeps the anchor"
    );
}

/// A test renderer: one counted call per render, scripted lines, a
/// distinctive styled span when asked, and an optional failure for
/// edit calls.
#[derive(Debug)]
struct Fake {
    calls: RefCell<usize>,
    per_input: RefCell<HashMap<String, usize>>,
    failed: RefCell<HashSet<String>>,
    lines: usize,
    styled: bool,
    fail_edits: bool,
    /// Every render fails under `extension`.
    fail_all: bool,
    /// Every render returns the wrong shape under `extension`.
    wrong_shape: bool,
    /// The `Failure.extension` a failure names.
    extension: String,
    /// One more slot the fake fills besides the catch-all.
    extra_slot: Option<String>,
}

impl Fake {
    /// `lines` plain `X` lines per render.
    fn lines(lines: usize) -> Self {
        Fake {
            calls: RefCell::new(0),
            per_input: RefCell::new(HashMap::new()),
            failed: RefCell::new(HashSet::new()),
            lines,
            styled: false,
            fail_edits: false,
            fail_all: false,
            wrong_shape: false,
            extension: "fake".to_owned(),
            extra_slot: None,
        }
    }

    /// A fake whose every render fails under `extension`.
    fn failing(extension: &str) -> Self {
        Fake {
            fail_all: true,
            extension: extension.to_owned(),
            ..Fake::lines(1)
        }
    }

    /// A fake whose every render returns the wrong shape under
    /// `extension`.
    fn misshapen(extension: &str) -> Self {
        Fake {
            wrong_shape: true,
            extension: extension.to_owned(),
            ..Fake::lines(1)
        }
    }
}

impl Renderer for Fake {
    fn name(&self) -> &str {
        &self.extension
    }

    fn fills(&self, slot: &str) -> bool {
        slot == "ledger_row" || self.extra_slot.as_deref() == Some(slot)
    }

    fn render(
        &self,
        _ctx: &crate::ext::SessionCtx,
        slot: &str,
        input: &Input<'_>,
        _width: u16,
        _builtin: &dyn Fn(&Input<'_>, u16) -> Vec<Vec<RawSpan>>,
    ) -> crate::ext::Decision {
        crate::ext::tests::decide(self.name(), || -> Result<Vec<Vec<RawSpan>>, Failure> {
            let key = format!("{slot} {input:?}");
            *self.calls.borrow_mut() += 1;
            *self.per_input.borrow_mut().entry(key.clone()).or_insert(0) += 1;
            if self.fail_all {
                self.failed.borrow_mut().insert(key);
                return Err(Failure {
                    kind: crate::ext::FailureKind::HookFailed,
                    extension: self.extension.clone(),
                    message: "always fails".to_owned(),
                });
            }
            if self.wrong_shape {
                return Ok(vec![vec![RawSpan {
                    text: "hi".to_owned(),
                    fg: Some("nope".to_owned()),
                    ..RawSpan::default()
                }]]);
            }
            if self.fail_edits && matches!(input, Input::LedgerRow(row) if row.name == "edit") {
                self.failed.borrow_mut().insert(key);
                return Err(Failure {
                    kind: crate::ext::FailureKind::HookFailed,
                    extension: "fake".to_owned(),
                    message: "edits stay built-in".to_owned(),
                });
            }
            let span = if self.styled {
                RawSpan {
                    text: "zxqy".to_owned(),
                    fg: Some("success".to_owned()),
                    bold: true,
                    ..RawSpan::default()
                }
            } else {
                RawSpan {
                    text: "X".to_owned(),
                    ..RawSpan::default()
                }
            };
            Ok(vec![vec![span]; self.lines])
        })
    }
}

/// Every resident page, in order.
fn resident(app: &App) -> Vec<usize> {
    (0..app.pages().index().pages().len())
        .filter(|at| app.pages().part(*at).is_some())
        .collect()
}

/// The search-style draw over every resident page: every ledger row
/// draws, open or not, so the renderer sees each distinct input once.
/// A normal draw skips closed groups' ledgers, so only this draw
/// reaches every input.
fn search(app: &App) {
    for at in resident(app) {
        app.pages().page_text_open_turns(at);
    }
}

/// The search-style draw over every resident page of `pages`: every
/// ledger row draws, open or not, so the renderer sees each distinct
/// input once.
fn search_pages(pages: &Pages) {
    for at in 0..pages.index().pages().len() {
        if pages.part(at).is_some() {
            pages.page_text_open_turns(at);
        }
    }
}

/// The renderer's per-input call counts, ordered by input: a stable
/// order for asserting over the unordered call map.
fn counts(fake: &Fake) -> Vec<(String, usize)> {
    let mut ordered: Vec<(String, usize)> = fake
        .per_input
        .borrow()
        .iter()
        .map(|(input, count)| (input.clone(), *count))
        .collect();
    ordered.sort();
    ordered
}

/// Renderer calls since the last count.
fn calls() -> usize {
    crate::work::take().renderer_calls
}

/// Scrolling redraws from the cache: after the first frame, ten redraws
/// at different tops and a Ctrl+O toggle add no calls; the search-style
/// draw calls once per distinct input not yet seen, and never twice.
#[test]
fn scrolling_and_searching_call_once_per_distinct_input() {
    let lines = session(2, true);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    let (top, _) = app.scroll();
    let height = app.conversation_height();
    // The first full frame, with no renderer installed: nothing to call.
    app.pages().shown(top, height);
    for delta in 0..10 {
        app.pages().shown(top + delta, height);
    }
    app.on_key(Key::CtrlO, now());
    assert_eq!(calls(), 0);
    // Installing the renderer caches nothing: the search-style draw
    // over every resident page calls once per distinct input.
    let fake = Rc::new(Fake::lines(1));
    app.install_renderer(fake.clone());
    search(&app);
    assert!(calls() > 0, "unseen inputs call");
    search(&app);
    assert_eq!(calls(), 0);
    assert!(
        fake.per_input.borrow().values().all(|count| *count == 1),
        "no input called twice"
    );
    // Redraws and a toggle hit the hot cache.
    for delta in 0..10 {
        app.pages().shown(top + delta, height);
    }
    app.on_key(Key::CtrlO, now());
    assert_eq!(calls(), 0);
}

/// A new page width calls every successful input again; back at the old
/// width they call a third time, while a failing input never calls again.
#[test]
fn a_new_width_calls_again_and_a_failure_never_calls_again() {
    let lines = session(2, false);
    let mut pages = Pages::new(80);
    let fake = Rc::new(Fake {
        fail_edits: true,
        ..Fake::lines(1)
    });
    for line in &lines {
        pages.apply(line);
    }
    // Installing after the fold starts the entries empty: transients
    // drawn while folding never reach the renderer.
    pages.ext.handle = Handle::create();
    pages.ext.handle.set(fake.clone());
    // The search-style draw calls once per distinct input: successes
    // cache their lines, edits fail and stay cached as failures.
    search_pages(&pages);
    assert!(calls() > 0, "the first width calls");
    assert!(!fake.failed.borrow().is_empty(), "an edit failed");
    assert!(
        fake.per_input.borrow().values().all(|count| *count == 1),
        "each input called once"
    );
    // A repeated search-style draw hits the hot cache.
    search_pages(&pages);
    assert_eq!(calls(), 0, "the hot cache calls nothing");
    // A new width drops the entries: every successful input calls
    // again, each failure stays cached.
    pages.set_width(100);
    search_pages(&pages);
    assert!(calls() > 0, "the new width calls again");
    for (input, count) in &counts(&fake) {
        if fake.failed.borrow().contains(input) {
            assert_eq!(*count, 1, "a failing input never calls again");
        } else {
            assert_eq!(*count, 2, "a new width calls again");
        }
    }
    // Back at the old width the entries are gone too: a third call.
    pages.set_width(80);
    search_pages(&pages);
    assert!(calls() > 0, "the old width calls again");
    for (input, count) in &counts(&fake) {
        if fake.failed.borrow().contains(input) {
            assert_eq!(*count, 1, "a failing input never calls again");
        } else {
            assert_eq!(*count, 3, "the old width calls again");
        }
    }
}

/// Cached lines keep the theme's role markers: two frames draw the same
/// rows with no new calls, every styled span carries an `Indexed`
/// marker, and no span carries an `Rgb` colour.
#[test]
fn cached_lines_keep_role_markers_across_frames() {
    use ratatui::style::Color;
    let lines = session(1, true);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    let fake = Rc::new(Fake {
        styled: true,
        ..Fake::lines(1)
    });
    app.install_renderer(fake.clone());
    search(&app);
    let first = calls();
    assert!(first > 0, "the first frame calls");
    // The look resolves the markers at paint time, so a second frame
    // under either look hits the same entries.
    search(&app);
    assert_eq!(calls(), 0);
    let mut marked = false;
    for at in resident(&app) {
        let (rows, ..) = app.pages().page_text_open_turns(at).expect("resident");
        for (line, _) in &rows {
            for span in &line.spans {
                assert!(!matches!(span.style.fg, Some(Color::Rgb(..))), "{span:?}");
                assert!(!matches!(span.style.bg, Some(Color::Rgb(..))), "{span:?}");
                marked = marked
                    || matches!(
                        span.style.fg,
                        Some(Color::Indexed(n)) if n == crate::theme::Role::Success as u8
                    );
            }
        }
    }
    assert!(marked, "a styled extension span drew");
}

/// Dropping a page drops its entries: reloading it calls its inputs
/// again once, while an undropped page costs nothing.
#[test]
fn dropping_a_page_calls_again_on_reload() {
    let lines = session(2, false);
    let mut pages = Pages::new(80);
    let fake = Rc::new(Fake::lines(1));
    for line in &lines {
        pages.apply(line);
    }
    // Installing after the fold starts the entries empty: transients
    // drawn while folding never reach the renderer.
    pages.ext.handle = Handle::create();
    pages.ext.handle.set(fake.clone());
    // The search-style draw caches every resident input once: a normal
    // draw skips closed groups' ledgers, so only this draw reaches
    // every input.
    search_pages(&pages);
    assert!(calls() > 0, "loading calls");
    assert!(
        fake.per_input.borrow().values().all(|count| *count == 1),
        "each input called once"
    );
    // Drop every closed page: the open page stays, and redrawing it
    // hits the hot cache.
    pages.trim(0, 0);
    let open = pages.residency.len();
    assert!(pages.part(open).is_some(), "the open page stays");
    pages.page_text_open_turns(open);
    assert_eq!(calls(), 0, "an undropped page does not call again");
    // Reloading a dropped page calls its inputs again, exactly once:
    // the reload counts without reaching closed ledgers, so the next
    // search-style draw misses each of its inputs once.
    let range = pages
        .index()
        .pages()
        .first()
        .map(|page| page.first_seq..=page.last_seq);
    if let Some(range) = range {
        pages.load(&SessionId(SESSION.to_owned()), &history(&lines, &range));
    }
    assert!(pages.part(0).is_some(), "page 0 loads back");
    search_pages(&pages);
    assert!(calls() > 0, "the reload calls again");
    assert!(
        fake.per_input.borrow().values().all(|count| *count <= 2),
        "no input called more than twice"
    );
    assert!(
        fake.per_input.borrow().values().any(|count| *count == 2),
        "a reloaded input called again"
    );
}

/// Dropping a page and reloading it restores its cards and its rows:
/// the closed page's part goes `Some` to `None` to `Some`, and the
/// reloaded page draws what it drew before the drop.
#[test]
fn dropping_a_page_reloads_it_with_identical_rows() {
    let lines = session(3, false);
    let mut pages = pages_at(&lines, 80);
    let at = 1;
    let before = pages.page_text_open(at).expect("page 1 is resident");
    let resident = pages.resident();
    assert!(pages.part(at).is_some(), "page 1 holds its cards");
    pages.trim(0, 0);
    assert!(pages.part(at).is_none(), "the drop clears the page");
    reload(&mut pages, &lines);
    assert!(pages.part(at).is_some(), "the page loads back");
    assert_eq!(
        pages.page_text_open(at).expect("reloaded page"),
        before,
        "a reloaded page draws its old rows"
    );
    assert_eq!(pages.resident(), resident, "every page is resident again");
}

/// A rewind stop uses a job description carried from an earlier page;
/// dropping and loading the page keeps the event's rendered name.
#[test]
fn reloading_a_rewind_page_keeps_stopped_jobs_from_earlier_pages() {
    let mut stream = Stream::new(false);
    stream.turn(0, 30);
    stream.durable(
        "job_started",
        None,
        json!({"job_id": "j_1", "description": "long-running task", "output_path": "/tmp/out"}),
    );
    stream.turn(1, 30);
    stream.durable(
        "job_completed",
        None,
        json!({"job_id": "j_1", "status": "cancelled"}),
    );
    stream.durable(
        "rewound",
        None,
        json!({"new_session_id": "s_ccc", "seq": 12}),
    );
    // A later group closes the page for this reload test. The production
    // stop run itself remains directly before `rewound`.
    stream.turn(2, 30);
    let lines = stream.lines;
    let mut pages = pages_at(&lines, 80);
    let at = (0..pages.index().pages().len())
        .find(|&at| {
            pages.page_text_open(at).is_some_and(|(rows, _)| {
                texts(&rows)
                    .iter()
                    .any(|line| line.contains("jobs stopped:"))
            })
        })
        .expect("a page contains the rewind line");
    assert!(
        at + 1 < pages.index().pages().len(),
        "the rewind page is closed"
    );
    let before = pages.page_text_open(at).expect("the page is resident");
    assert!(
        texts(&before.0)
            .iter()
            .any(|line| line.contains("long-running task")),
        "the stop line uses the carried description"
    );

    pages.trim(0, 0);
    assert!(pages.part(at).is_none(), "the stop page was dropped");
    reload(&mut pages, &lines);
    let after = pages.page_text_open(at).expect("the page reloads");
    assert!(
        texts(&after.0)
            .iter()
            .any(|line| line.contains("long-running task")),
        "the reloaded stop line keeps the carried description: {after:?}"
    );
}

/// An orphaned job whose start is on an earlier page keeps its
/// description when its page reloads.
#[test]
fn reloading_an_orphan_page_keeps_a_carried_description() {
    let mut stream = Stream::new(false);
    stream.turn(0, 30);
    stream.durable(
        "job_started",
        None,
        json!({"job_id": "j_9", "description": "doomed task", "output_path": "/tmp/out"}),
    );
    // A forced cut puts the completion on the next page: the start
    // stays behind, so the reload reads the name from the seed.
    fill(&mut stream, 70);
    begun(&mut stream, "next");
    completed(&mut stream);
    stream.durable(
        "job_completed",
        None,
        json!({"job_id": "j_9", "status": "failed",
            "error": {"code": "orphaned", "message": "lost nine"}}),
    );
    stream.turn(2, 30);
    let lines = stream.lines;
    let started = lines
        .iter()
        .find(|line| line.kind == "job_started")
        .expect("a start")
        .seq
        .expect("durable");
    let done = lines
        .iter()
        .find(|line| line.kind == "job_completed")
        .expect("a completion")
        .seq
        .expect("durable");
    let mut pages = pages_at(&lines, 80);
    let session = SessionId(SESSION.to_owned());
    assert!(
        pages.index().page_of(&session, started) < pages.index().page_of(&session, done),
        "the start stays on an earlier page"
    );
    let at = (0..pages.index().pages().len())
        .find(|&at| {
            pages.page_text_open(at).is_some_and(|(rows, _)| {
                texts(&rows)
                    .iter()
                    .any(|line| line.contains("Orphaned jobs:"))
            })
        })
        .expect("a page contains the orphan line");
    assert!(
        at + 1 < pages.index().pages().len(),
        "the orphan page is closed"
    );
    let before = pages.page_text_open(at).expect("the page is resident");
    assert!(
        texts(&before.0)
            .iter()
            .any(|line| line.contains("doomed task")),
        "the orphan line uses the carried description"
    );

    pages.trim(0, 0);
    assert!(pages.part(at).is_none(), "the orphan page was dropped");
    reload(&mut pages, &lines);
    let after = pages.page_text_open(at).expect("the page reloads");
    assert!(
        texts(&after.0)
            .iter()
            .any(|line| line.contains("doomed task")),
        "the reloaded orphan line keeps the carried description: {after:?}"
    );
}

/// A page cut between a stop and `rewound` ends the run on every path:
/// the reloaded stop line agrees with the live one.
#[test]
fn a_cut_between_the_stop_and_rewound_drops_the_stop_on_reload() {
    let mut stream = Stream::new(false);
    stream.turn(0, 30);
    stream.durable(
        "job_started",
        None,
        json!({"job_id": "j_1", "description": "long-running task", "output_path": "/tmp/out"}),
    );
    stream.turn(1, 30);
    stream.durable(
        "job_completed",
        None,
        json!({"job_id": "j_1", "status": "cancelled"}),
    );
    // Filler and a new turn force the cut after the stop: the run is
    // over, so `rewound` stands alone on the next page.
    fill(&mut stream, 70);
    begun(&mut stream, "next");
    completed(&mut stream);
    stream.durable(
        "rewound",
        None,
        json!({"new_session_id": "s_ccc", "seq": 12}),
    );
    stream.turn(2, 30);
    let lines = stream.lines;
    let stop = lines
        .iter()
        .find(|line| line.kind == "job_completed")
        .expect("a stop")
        .seq
        .expect("durable");
    let wound = lines
        .iter()
        .find(|line| line.kind == "rewound")
        .expect("a rewound")
        .seq
        .expect("durable");
    let mut pages = pages_at(&lines, 80);
    let session = SessionId(SESSION.to_owned());
    assert!(
        pages.index().page_of(&session, stop) < pages.index().page_of(&session, wound),
        "the stop is on the page before the rewind line"
    );
    let at = (0..pages.index().pages().len())
        .find(|&at| {
            pages.page_text_open(at).is_some_and(|(rows, _)| {
                texts(&rows)
                    .iter()
                    .any(|line| line.contains("jobs stopped:"))
            })
        })
        .expect("a page contains the rewind line");
    assert!(
        at + 1 < pages.index().pages().len(),
        "the rewind page is closed"
    );
    // The cut broke the run: the live line already drops the earlier
    // page's job.
    let before = pages.page_text_open(at).expect("the page is resident");
    assert!(
        texts(&before.0)
            .iter()
            .any(|line| line.contains("jobs stopped: none")),
        "the live stop line drops the broken run: {before:?}"
    );

    pages.trim(0, 0);
    assert!(pages.part(at).is_none(), "the stop page was dropped");
    reload(&mut pages, &lines);
    let after = pages.page_text_open(at).expect("the page reloads");
    assert!(
        texts(&after.0)
            .iter()
            .any(|line| line.contains("jobs stopped: none")),
        "the reloaded stop line drops the broken run: {after:?}"
    );
    assert!(
        !texts(&after.0)
            .iter()
            .any(|line| line.contains("long-running task")),
        "the broken run stays dropped: {after:?}"
    );
}

/// Installing a renderer recounts every page under its lines, and
/// replacing it recounts again.
#[test]
fn installing_a_renderer_recounts_under_its_lines() {
    let lines = session(1, true);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    // Open every ledger first: the recount draws only open ledgers,
    // so closed ones would stay silent under any renderer.
    app.on_key(Key::CtrlO, now());
    let rows = |app: &App| {
        app.pages()
            .index()
            .pages()
            .iter()
            .map(|page| page.rows)
            .collect::<Vec<_>>()
    };
    let before = rows(&app);
    // Two lines per ledger row: every ledger row grows the counts.
    let two = Rc::new(Fake::lines(2));
    app.use_renderer(two.clone());
    assert!(*two.calls.borrow() > 0, "the install recounts");
    let grown = rows(&app);
    assert!(grown.iter().sum::<usize>() > before.iter().sum::<usize>());
    // One line per row again: the counts come back.
    let one = Rc::new(Fake::lines(1));
    app.use_renderer(one.clone());
    assert!(*one.calls.borrow() > 0, "replacing recounts");
    assert_eq!(rows(&app), before);
}

/// A fresh copy draws through the same install with an empty cache,
/// and copies the flag.
#[test]
fn a_fresh_copy_shares_the_renderer_with_an_empty_cache() {
    let lines = session(1, false);
    let mut pages = Pages::new(80);
    let fake = Rc::new(Fake::lines(1));
    pages.ext.handle = Handle::create();
    pages.ext.handle.set(fake.clone());
    pages.set_flat(true);
    for line in &lines {
        pages.apply(line);
    }
    search_pages(&pages);
    assert!(calls() > 0, "the session draws");
    // The copy draws through the same install with an empty cache: the
    // same fake is called, once per distinct input, then never again.
    // Folding the lines into the copy is what the land and tail swaps
    // do; the copy carries no rows of its own.
    let mut fresh = pages.fresh();
    assert!(fresh.ext.flat, "the copy keeps the flag");
    for line in &lines {
        fresh.apply(line);
    }
    calls();
    search_pages(&fresh);
    assert!(calls() > 0, "an empty cache calls");
    search_pages(&fresh);
    assert_eq!(calls(), 0, "the second draw hits");
}

/// The notice text of every shown notice, oldest first.
#[test]
fn session_gate_recounts_only_matching_residents_and_stales_dropped_pages() {
    let lines = session(3, false);
    let mut pages = Pages::new(80);
    pages.set_flat(true);
    let mut fake = crate::ext::tests::SessionRenderer::new("GATED-ROWS");
    fake.gated = true;
    pages.ext.handle = Handle::create();
    pages.ext.handle.set(Rc::new(fake));
    for line in &lines {
        pages.apply(line);
    }
    pages.toggle_ledgers(); // Count visible ledger rows, not their unchanged summary.
    let session = lines[0].session_id.clone();
    let before: Vec<_> = pages.index().pages().iter().map(|p| p.rows).collect();
    let recounts = pages.recounts;
    pages.record_halves(
        &contract::SessionId("unrelated".into()),
        [("session-test".into(), "v1.0.0".into())].into(),
    );
    assert_eq!(
        pages.recounts, recounts,
        "unrelated sessions do not recount"
    );
    pages.record_halves(&session, [("session-test".into(), "v1.0.0".into())].into());
    assert!(pages.recounts > recounts);
    assert_ne!(
        before,
        pages
            .index()
            .pages()
            .iter()
            .map(|p| p.rows)
            .collect::<Vec<_>>()
    );
    assert!(pages.index().pages().iter().enumerate().any(|(at, _)| {
        pages.page_text_open(at).is_some_and(|(rows, _)| {
            rows.iter()
                .any(|(line, _)| line.to_string().contains("GATED-ROWS"))
        })
    }));
    pages.trim(0, 0);
    let dropped = (0..pages.index().pages().len())
        .find(|at| pages.part(*at).is_none())
        .expect("fixture drops a page");
    pages.record_halves(&session, std::collections::BTreeMap::new());
    assert!(pages.residency.stale().contains(&dropped));
    let page = pages.index().pages()[dropped].clone();
    let reload: Vec<_> = lines
        .iter()
        .filter(|line| {
            line.seq
                .is_some_and(|seq| seq >= page.first_seq && seq <= page.last_seq)
        })
        .cloned()
        .collect();
    pages.load(&session, &reload);
    let (rows, _) = pages.page_text_open(dropped).expect("reloaded page");
    assert!(
        !rows
            .iter()
            .any(|(line, _)| line.to_string().contains("GATED-ROWS"))
    );
    assert!(!pages.residency.stale().contains(&dropped));
}

#[test]
fn unrelated_halves_keep_entries_and_failures_but_selected_half_changes_retry() {
    for failure in [None, Some(crate::ext::FailureKind::HookFailed)] {
        let lines = session(1, false);
        let mut pages = Pages::new(80);
        let mut fake = crate::ext::tests::SessionRenderer::new("HALF-KEY");
        fake.gated = true;
        fake.failure = failure;
        let fake = Rc::new(fake);
        pages.ext.handle = Handle::create();
        pages.ext.handle.set(fake.clone());
        for line in &lines {
            pages.apply(line);
        }
        let mut halves =
            std::collections::BTreeMap::from([("session-test".into(), "v1.0.0".into())]);
        pages.record_halves(&lines[0].session_id, halves.clone());
        search_pages(&pages);
        let first = fake.calls.borrow().len();
        assert!(first > 0);
        halves.insert("unrelated".into(), "v9.0.0".into());
        pages.record_halves(&lines[0].session_id, halves.clone());
        search_pages(&pages);
        assert_eq!(
            fake.calls.borrow().len(),
            first,
            "unrelated map updates keep the selected identity"
        );
        halves.insert("session-test".into(), "v1.1.0".into());
        pages.record_halves(&lines[0].session_id, halves);
        search_pages(&pages);
        assert_eq!(
            fake.calls.borrow().len(),
            first * 2,
            "relevant half changes miss entries and width-independent failure memo"
        );
    }
}

#[test]
fn dropped_page_selection_notice_survives_scratch_without_a_render_call() {
    let lines = session(3, false);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    let mut fake = crate::ext::tests::SessionRenderer::new("unseen");
    fake.gated = true;
    let fake = Rc::new(fake);
    app.install_renderer(fake.clone());
    let mut fresh = app.pages().fresh();
    for line in &lines {
        fresh.apply(line);
    }
    fresh.trim(0, 0);
    let at = (0..fresh.index().pages().len())
        .find(|at| fresh.part(*at).is_none())
        .unwrap();
    fresh.record_halves(&lines[0].session_id, std::collections::BTreeMap::new());
    let page = &fresh.index().pages()[at];
    let data = history(&lines, &(page.first_seq..=page.last_seq));
    fresh.fold_text(at, &data);
    fresh.fold_text(at, &data);
    drop(fresh);
    assert!(
        fake.calls.borrow().is_empty(),
        "incompatible selection never calls render"
    );
    assert!(app.drawn(&[]));
    let notices = notice_texts(&app);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, "extension_incompatible");
    assert!(notices[0].1.contains("not loaded"));
    assert!(!app.drawn(&[]));
}

#[test]
fn dropped_and_speculative_draws_deliver_failures_once_to_app() {
    for kind in [
        crate::ext::FailureKind::HookFailed,
        crate::ext::FailureKind::ExtensionFailed,
    ] {
        let lines = session(3, false);
        let mut app = app(80, 20);
        open(&mut app, &lines);
        let mut fake = crate::ext::tests::SessionRenderer::new("failure");
        fake.failure = Some(kind);
        let fake = Rc::new(fake);
        app.install_renderer(fake.clone());
        let mut fresh = app.pages().fresh();
        for line in &lines {
            fresh.apply(line);
        }
        fresh.trim(0, 0);
        let dropped = (0..fresh.index().pages().len())
            .find(|at| fresh.part(*at).is_none())
            .unwrap();
        let page = fresh.index().pages()[dropped].clone();
        let data: Vec<_> = lines
            .iter()
            .filter(|line| {
                line.seq
                    .is_some_and(|seq| seq >= page.first_seq && seq <= page.last_seq)
            })
            .cloned()
            .collect();
        fresh.fold_text(dropped, &data);
        let calls = fake.calls.borrow().len();
        assert!(calls > 0);
        fresh.fold_text(dropped, &data);
        assert_eq!(
            fake.calls.borrow().len(),
            calls,
            "scratch draws share failure memo"
        );
        drop(fresh);
        assert!(app.drawn(&[]));
        let notices = notice_texts(&app);
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0].0,
            if kind == crate::ext::FailureKind::HookFailed {
                "hook_failed"
            } else {
                "extension_failed"
            }
        );
        assert!(notices[0].1.contains("session-test"));
        assert!(notices[0].1.contains("ledger_row"));
        assert!(!app.drawn(&[]));
    }
}

fn notice_texts(app: &App) -> Vec<(String, String)> {
    app.notices()
        .into_iter()
        .filter_map(|line| match line {
            crate::app::NoticeLine::Notice { code, text, .. } => Some((code, text)),
            crate::app::NoticeLine::More(_) => None,
        })
        .collect()
}

/// Fifty failing ledger rows in one slot report exactly one notice:
/// it names the extension, the slot and the message under
/// `hook_failed`. A repeated draw calls nothing, adds none, and draws
/// the frame again once, then not again.
#[test]
fn many_failures_in_one_slot_report_exactly_one_notice() {
    let lines = session(2, true);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    app.install_renderer(Rc::new(Fake::failing("fake")));
    // Walk the whole session so every ledger row fails once.
    while app.scroll().0 > 0 {
        search(&app);
        app.on_key(Key::PageUp, now());
        frame(&mut app, &lines);
    }
    search(&app);
    let failed = calls();
    assert!(failed >= 50, "fifty rows fail, got {failed}");
    // A new failure draws the frame again with its notice.
    assert!(app.drawn(&[]), "a new failure draws again");
    assert_eq!(
        notice_texts(&app),
        [(
            "hook_failed".to_owned(),
            "extension fake could not draw ledger_row: always fails; \
             the built-in row is drawn"
                .to_owned(),
        )]
    );
    // A cached failure on a repeated draw adds none, and the frame
    // with no new notice draws only once.
    search(&app);
    assert_eq!(calls(), 0, "cached failures call nothing");
    assert!(!app.drawn(&[]), "no new failure draws nothing again");
    assert!(!app.drawn(&[]), "still nothing new");
    assert_eq!(notice_texts(&app).len(), 1);
}

/// The reported set is keyed by extension and slot: the same extension
/// failing again adds nothing, a second extension in the same slot and
/// a second slot each report again.
#[test]
fn a_second_extension_and_a_second_slot_report_again() {
    let lines = session(2, true);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    app.use_renderer(Rc::new(Fake::failing("alpha")));
    search(&app);
    assert!(app.drawn(&[]));
    assert_eq!(notice_texts(&app).len(), 1);
    // The same extension failing again reports nothing: its pair is
    // already in the set, even across a reinstall that clears the cache.
    app.use_renderer(Rc::new(Fake::failing("alpha")));
    search(&app);
    assert!(!app.drawn(&[]), "a reported pair adds nothing");
    assert_eq!(notice_texts(&app).len(), 1);
    // A second extension in the same slot reports again.
    app.use_renderer(Rc::new(Fake::failing("beta")));
    search(&app);
    assert!(app.drawn(&[]));
    let texts = notice_texts(&app);
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(
        texts[1]
            .1
            .contains("extension beta could not draw ledger_row:"),
        "{texts:?}"
    );
    // A second slot under the first extension reports again, while the
    // already-reported slot stays silent.
    let mut shell = Fake::failing("alpha");
    shell.extra_slot = Some("ledger_row:read".to_owned());
    app.use_renderer(Rc::new(shell));
    search(&app);
    assert!(app.drawn(&[]));
    let texts = notice_texts(&app);
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert!(
        texts[2].1.contains("could not draw ledger_row:read:"),
        "{texts:?}"
    );
}

/// Two wrong shapes in one slot report twice: the failure names the
/// extension that returned it, so a second extension's wrong shape is
/// a new pair even though the slot already reported.
#[test]
fn two_wrong_shapes_in_one_slot_report_twice() {
    let lines = session(1, true);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    app.use_renderer(Rc::new(Fake::misshapen("alpha")));
    search(&app);
    assert!(app.drawn(&[]));
    let texts = notice_texts(&app);
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(
        texts[0]
            .1
            .contains("extension alpha could not draw ledger_row:"),
        "{texts:?}"
    );
    // A second extension returning the wrong shape in the same slot
    // reports again: its pair was never named.
    app.use_renderer(Rc::new(Fake::misshapen("beta")));
    search(&app);
    assert!(app.drawn(&[]));
    let texts = notice_texts(&app);
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(
        texts[1]
            .1
            .contains("extension beta could not draw ledger_row:"),
        "{texts:?}"
    );
}

/// The reload command clears the reported set through the slash path:
/// the same extension failing again dedupes onto the standing row.
#[test]
fn reload_reports_the_same_failure_again() {
    let lines = session(1, true);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    app.use_renderer(Rc::new(Fake::failing("alpha")));
    search(&app);
    assert!(app.drawn(&[]));
    assert_eq!(notice_texts(&app).len(), 1);
    for ch in "/reload".chars() {
        app.on_key(Key::Char(ch), now());
    }
    let _ = app.on_key(Key::Enter, now());
    // The same extension failing again adds no row: the re-failure's
    // push dedupes onto the standing row, which stays until it fades
    // or ✕ dismisses it.
    app.use_renderer(Rc::new(Fake::failing("alpha")));
    search(&app);
    assert!(app.drawn(&[]));
    let texts = notice_texts(&app);
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(
        texts[0]
            .1
            .contains("extension alpha could not draw ledger_row:"),
        "{texts:?}"
    );
}

/// The page's bound seam shares the install and its cache: the first draw
/// calls once per distinct input and carries the renderer's lines, the
/// second draw calls nothing.
#[test]
fn bound_shares_the_renderer_and_its_cache_with_the_page() {
    // Kills crates/tui/src/ext.rs:58 replace Ext::bind -> Bound with
    // Default::default(): the default Bound carries no handle and a fresh
    // cache, so the renderer is never called and no row carries its lines.
    // The phase-one `> 0` / non-empty / `zxqy` asserts fail with the mutant
    // applied, and the phase-two non-empty / `zxqy` asserts fail with it
    // too, since the totals stay at zero; no run needed.
    let lines = session(1, true);
    let mut app = app(80, 20);
    open(&mut app, &lines);
    app.on_key(Key::CtrlO, now());
    let fake = Rc::new(Fake {
        styled: true,
        ..Fake::lines(1)
    });
    // The install recounts every page through the bound seam: each distinct
    // ledger input calls exactly once.
    app.use_renderer(fake.clone());
    let drawn: Vec<String> = resident(&app)
        .iter()
        .flat_map(|at| {
            app.pages()
                .page_text_open_turns(*at)
                .map(|(rows, ..)| drawn_texts(&rows))
                .unwrap_or_default()
        })
        .collect();
    assert!(calls() > 0, "the bound renderer draws");
    assert!(
        !fake.per_input.borrow().is_empty(),
        "the renderer saw an input"
    );
    assert!(
        fake.per_input.borrow().values().all(|count| *count == 1),
        "each input called once"
    );
    assert!(
        drawn.iter().any(|line| line.contains("zxqy")),
        "a row carries the renderer's lines: {drawn:?}"
    );
    let total = *fake.calls.borrow();
    assert!(total > 0, "the renderer was called");
    // The same draw again hits the bound clone's shared cache: nothing
    // calls again and the rows keep the renderer's lines.
    search(&app);
    assert_eq!(calls(), 0, "the second draw hits the bound cache");
    assert_eq!(*fake.calls.borrow(), total, "the second draw calls nothing");
    assert!(
        !fake.per_input.borrow().is_empty(),
        "the renderer still saw an input"
    );
    assert!(
        fake.per_input.borrow().values().all(|count| *count == 1),
        "no input called twice"
    );
    let again: Vec<String> = resident(&app)
        .iter()
        .flat_map(|at| {
            app.pages()
                .page_text_open_turns(*at)
                .map(|(rows, ..)| drawn_texts(&rows))
                .unwrap_or_default()
        })
        .collect();
    assert!(
        again.iter().any(|line| line.contains("zxqy")),
        "a row still carries the renderer's lines: {again:?}"
    );
}

/// The ancestor's session: its lines replay under this id.
const ANCESTOR: &str = "s_bbbbbbbbbbbbbbbb";

/// `lines` rehomed under `session`: the ancestor's log under its own id.
fn rehome(lines: Vec<Envelope>, session: &str) -> Vec<Envelope> {
    lines
        .into_iter()
        .map(|mut line| {
            line.session_id = SessionId(session.to_owned());
            line
        })
        .collect()
}

/// One turn's durable lines under `session`, numbered from `seq`: turn
/// `turn`, so the two segments' calls and generations stay apart.
fn segment_turn(session: &str, seq: &mut u64, turn: usize) -> Vec<Envelope> {
    let mut stream = Stream::new(false);
    stream.seq = *seq;
    stream.turn(turn, 30);
    *seq = stream.seq;
    rehome(std::mem::take(&mut stream.lines), session)
}

/// A two-segment log: the ancestor's turns, then the own run's turns,
/// each segment numbered from zero.
fn chain() -> (Vec<Envelope>, Vec<Envelope>) {
    let mut aseq = 0u64;
    let mut ancestor = segment_turn(ANCESTOR, &mut aseq, 0);
    ancestor.extend(segment_turn(ANCESTOR, &mut aseq, 1));
    let mut dseq = 0u64;
    let mut log = ancestor.clone();
    log.extend(segment_turn(SESSION, &mut dseq, 10));
    log.extend(segment_turn(SESSION, &mut dseq, 11));
    assert!(ancestor.len() > 64, "the ancestor spans pages");
    (ancestor, log)
}

/// Every page's lines joined, loading each dropped page under its own
/// session from `log`, and checking each page's count against the rows
/// its lines draw.
fn chain_joined(pages: &mut Pages, log: &[Envelope]) -> Vec<Row> {
    let mut out = Vec::new();
    for at in 0..pages.index().pages().len() {
        if pages.part(at).is_none() {
            let page = pages
                .index()
                .pages()
                .get(at)
                .cloned()
                .unwrap_or_else(|| panic!("page {at}"));
            let lines: Vec<Envelope> = log
                .iter()
                .filter(|line| {
                    line.session_id == page.session
                        && line
                            .seq
                            .is_some_and(|seq| (page.first_seq..=page.last_seq).contains(&seq))
                })
                .cloned()
                .collect();
            assert!(!lines.is_empty(), "no lines for page {at}");
            pages.load(&page.session, &lines);
        }
        assert!(pages.part(at).is_some(), "page {at} did not load");
        let mut rows = Rows::default();
        if let Some(part) = pages.part(at) {
            pages.draw(at, part, &mut rows);
        }
        let drawn: usize = rows
            .iter()
            .map(|(line, _)| crate::view::rows(line, pages.width))
            .sum();
        let counted = pages.index().pages().get(at).map(|page| page.rows);
        assert_eq!(counted, Some(drawn), "page {at}");
        out.extend(rows.into_parts().0);
    }
    out
}

#[test]
fn chain_pages_load_under_their_own_session_and_count_the_chain() {
    let (_ancestor, log) = chain();
    let mut pages = Pages::new(80);
    for line in &log {
        pages.apply(line);
    }
    // One page never spans the continuation point: every page carries
    // the session of the lines it folded.
    let seam = pages
        .index()
        .pages()
        .iter()
        .position(|page| page.session == SessionId(SESSION.to_owned()))
        .expect("the own run");
    assert!(seam > 0, "the ancestor runs first");
    assert!(
        pages.index().pages()[..seam]
            .iter()
            .all(|page| page.session == SessionId(ANCESTOR.to_owned())),
        "every page before the seam is the ancestor's"
    );
    assert!(
        pages.index().pages()[seam..]
            .iter()
            .all(|page| page.session == SessionId(SESSION.to_owned())),
        "every page from the seam on is the own run's"
    );
    let joint = pages.index().total();
    let reference = whole(&pages, &log);
    // The live cards draw the chain seamless.
    let got = chain_joined(&mut pages, &log);
    assert_eq!(differs(&got, &reference), None);
    // Dropping every closed page re-asks each under its own session.
    drop_all(&mut pages);
    let needs = pages.needs(0, top_total(&pages));
    assert_eq!(
        needs.len(),
        pages.index().pages().len() - 1,
        "every closed page is needed"
    );
    for (session, range) in &needs {
        let page = pages
            .index()
            .pages()
            .iter()
            .find(|page| page.session == *session && (page.first_seq..=page.last_seq) == *range)
            .unwrap_or_else(|| panic!("{session:?} {range:?} names a page"));
        let lines: Vec<Envelope> = log
            .iter()
            .filter(|line| {
                line.session_id == page.session
                    && line
                        .seq
                        .is_some_and(|seq| (page.first_seq..=page.last_seq).contains(&seq))
            })
            .cloned()
            .collect();
        assert!(!lines.is_empty(), "no lines for {session:?} {range:?}");
        pages.load(session, &lines);
    }
    assert!(pages.needs(0, top_total(&pages)).is_empty());
    assert_eq!(
        pages.index().total(),
        joint,
        "the joint count survives the reload"
    );
    // Folded back, the chain draws what one fold draws.
    let got = chain_joined(&mut pages, &log);
    assert_eq!(differs(&got, &reference), None);
}

/// The open page's row count, as a window tall enough to need it all.
fn top_total(pages: &Pages) -> usize {
    pages.index().total()
}

#[test]
fn a_failed_ancestor_page_keeps_every_count() {
    let (_ancestor, log) = chain();
    let mut pages = Pages::new(80);
    for line in &log {
        pages.apply(line);
    }
    let joint = pages.index().total();
    let seam = pages
        .index()
        .pages()
        .iter()
        .position(|page| page.session == SessionId(SESSION.to_owned()))
        .expect("the own run starts past a seam");
    let failed = pages.index().pages()[seam - 1].clone();
    assert_eq!(
        failed.session,
        SessionId(ANCESTOR.to_owned()),
        "the page before the seam is the ancestor's"
    );
    drop_all(&mut pages);
    // Failing the ancestor's last page forgets its ask, nothing else.
    pages.fail(&failed.session, failed.first_seq);
    let top = pages.index().total();
    let needs = pages.needs(0, top);
    assert!(
        !needs.contains(&(failed.session.clone(), failed.first_seq..=failed.last_seq)),
        "the failed ask is gone"
    );
    assert_eq!(
        needs.len(),
        pages.index().pages().len() - 2,
        "every other closed page is still needed"
    );
    // Everything else loads; the failed page draws blank, rows kept.
    let got = chain_joined_except(&mut pages, &log, seam - 1);
    assert!(pages.needs(0, top).is_empty());
    assert_eq!(
        pages.index().total(),
        joint,
        "the joint count survives the failure"
    );
    assert!(pages.part(seam - 1).is_none());
    assert!(
        got.len() < whole(&pages, &log).len(),
        "the failed page draws nothing"
    );
}

/// Only a closed page's load can fail: the open page holds the live
/// fold and never loads, and an unknown page holds nothing. Kills the
/// `at >= len` guard's removal (a pass-through would report the open
/// page failed).
#[test]
fn page_failed_names_only_a_closed_page_failure() {
    let (_ancestor, log) = chain();
    let mut pages = Pages::new(80);
    for line in &log {
        pages.apply(line);
    }
    let open = pages.residency.len();
    assert!(open > 0, "the chain closes a page");
    let failed = pages.index().pages()[0].clone();
    drop_all(&mut pages);
    pages.fail(&failed.session, failed.first_seq);
    assert!(pages.page_failed(0), "a closed page's failure reports");
    // The open page never loads: even with its index in the failed set
    // it reports nothing, as does an unknown page.
    pages.residency.failed_mut().insert(open);
    assert!(!pages.page_failed(open), "the open page never fails");
    assert!(!pages.page_failed(open + 1), "an unknown page never fails");
}

/// A width change clears what failed since the last one: the failed
/// page is asked for again. Kills `replace Residency::on_width_change
/// with ()`.
#[test]
fn a_width_change_asks_for_a_failed_page_again() {
    let (_ancestor, log) = chain();
    let mut pages = Pages::new(80);
    for line in &log {
        pages.apply(line);
    }
    let seam = pages
        .index()
        .pages()
        .iter()
        .position(|page| page.session == SessionId(SESSION.to_owned()))
        .expect("the own run starts past a seam");
    let failed = pages.index().pages()[seam - 1].clone();
    drop_all(&mut pages);
    pages.fail(&failed.session, failed.first_seq);
    assert!(
        pages.page_failed(seam - 1),
        "the failure reports before the width change"
    );
    let top = pages.index().total();
    assert!(
        !pages
            .needs(0, top)
            .contains(&(failed.session.clone(), failed.first_seq..=failed.last_seq)),
        "the failed ask is gone"
    );
    pages.set_width(100);
    assert!(
        !pages.page_failed(seam - 1),
        "a new width clears the failure"
    );
    let top = pages.index().total();
    assert!(
        pages
            .needs(0, top)
            .contains(&(failed.session.clone(), failed.first_seq..=failed.last_seq)),
        "the failed ask returns"
    );
}

/// Like [`chain_joined`], but page `skip` stays dropped.
fn chain_joined_except(pages: &mut Pages, log: &[Envelope], skip: usize) -> Vec<Row> {
    let mut out = Vec::new();
    for at in 0..pages.index().pages().len() {
        if at == skip {
            continue;
        }
        if pages.part(at).is_none() {
            let page = pages
                .index()
                .pages()
                .get(at)
                .cloned()
                .unwrap_or_else(|| panic!("page {at}"));
            let lines: Vec<Envelope> = log
                .iter()
                .filter(|line| {
                    line.session_id == page.session
                        && line
                            .seq
                            .is_some_and(|seq| (page.first_seq..=page.last_seq).contains(&seq))
                })
                .cloned()
                .collect();
            assert!(!lines.is_empty(), "no lines for page {at}");
            pages.load(&page.session, &lines);
        }
        assert!(pages.part(at).is_some(), "page {at} did not load");
        let mut rows = Rows::default();
        if let Some(part) = pages.part(at) {
            pages.draw(at, part, &mut rows);
        }
        out.extend(rows.into_parts().0);
    }
    out
}

/// One durable line under `session` at `seq`, for the carry-chain edge
/// tests: turns open with a text prompt, everything else is bare.
fn edge_line(session: &str, seq: u64, kind: &str) -> Envelope {
    let payload = if kind == "turn_started" {
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "hi"}]}]})
    } else {
        json!({})
    };
    Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: seq,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(Seq(seq)),
        payload: payload.as_object().cloned().unwrap_or_default(),
    }
}

/// A preview holding three closed ancestor pages and one closed own page,
/// with the open page on the own run, and a fold holding one closed own
/// page. Returns the pair with the carried page count and the carried
/// summary count, both asserted so a fixture drift fails loudly instead
/// of weakening the edges below.
fn carry_edge() -> (Pages, Pages, usize, usize) {
    let mut preview = Pages::new(80);
    for seq in 0..64 {
        preview.apply(&edge_line(ANCESTOR, seq, "usage_recorded"));
    }
    preview.apply(&edge_line(ANCESTOR, 64, "turn_started"));
    for seq in 65..129 {
        preview.apply(&edge_line(ANCESTOR, seq, "usage_recorded"));
    }
    preview.apply(&edge_line(ANCESTOR, 129, "turn_started"));
    // The child's first line cuts the session onto a new page, which the
    // open holds alone: the land path's session-change close.
    preview.apply(&edge_line(SESSION, 0, "session_started"));
    for seq in 1..66 {
        preview.apply(&edge_line(SESSION, seq, "usage_recorded"));
    }
    preview.apply(&edge_line(SESSION, 66, "turn_started"));
    let seam = preview
        .index()
        .pages()
        .iter()
        .position(|page| page.session == SessionId(SESSION.to_owned()))
        .expect("the own run");
    assert_eq!(seam, 3, "three closed ancestor pages precede the seam");
    assert_eq!(preview.page_count(), 5, "four closes and the open page");
    let attached = preview
        .index()
        .pages()
        .last()
        .map(|page| page.session.clone())
        .expect("the open page");
    let len = preview
        .index()
        .pages()
        .iter()
        .take(preview.page_count() - 1)
        .take_while(|page| page.session != attached)
        .count();
    assert_eq!(len, 3, "the carried ancestor run");
    // Mirrors `carry_chain`'s carried summary count.
    let mut summaries = 0;
    for at in 0..len {
        summaries = summaries.max(preview.page_first(at).unwrap_or(0).saturating_add(1));
        if let Some(part) = preview.part(at) {
            summaries = summaries.max(part.first.saturating_add(part.turns.len().max(1)));
        }
    }
    summaries = summaries.min(preview.summaries.len());
    assert_eq!(summaries, 2, "the two ancestor turns");
    let mut fold = Pages::new(80);
    for seq in 0..64 {
        fold.apply(&edge_line(SESSION, seq, "usage_recorded"));
    }
    fold.apply(&edge_line(SESSION, 64, "turn_started"));
    (preview, fold, len, summaries)
}

/// `carry_chain` carries only generations below the carried summaries:
/// the fold's own move with them, and the entry at the boundary stays
/// behind. Kills every `<` mutant on the generations guard (`<=` keeps
/// the boundary, `==` and `>` drop the carried one).
#[test]
fn carry_chain_carries_only_generations_below_the_carried_summaries() {
    let (mut preview, mut fold, _len, summaries) = carry_edge();
    preview
        .generations
        .insert(contract::GenerationId("g_in".to_owned()), 0);
    preview
        .generations
        .insert(contract::GenerationId("g_edge".to_owned()), summaries);
    fold.generations
        .insert(contract::GenerationId("g_fold".to_owned()), 0);
    preview.carry_chain(&mut fold);
    assert_eq!(
        fold.generations
            .get(&contract::GenerationId("g_in".to_owned())),
        Some(&0),
        "the carried generation keeps its turn"
    );
    assert!(
        !fold
            .generations
            .contains_key(&contract::GenerationId("g_edge".to_owned())),
        "the generation at the boundary stays behind"
    );
    assert_eq!(
        fold.generations
            .get(&contract::GenerationId("g_fold".to_owned())),
        Some(&summaries),
        "the fold's own generation moves with the summaries"
    );
}

/// `carry_chain` carries only shells below the carried count: the
/// fold's own shift by it, and the shell at the boundary stays behind.
/// Kills every `<` mutant on the shells filter.
#[test]
fn carry_chain_carries_only_shells_below_the_carried_count() {
    let (mut preview, mut fold, len, summaries) = carry_edge();
    preview.shells.push((0, 0, "ancestor output".to_owned()));
    preview.shell_index.entry((0, 0)).or_default().push(0);
    preview.shells.push((len, 1, "own output".to_owned()));
    preview.shell_index.entry((len, 1)).or_default().push(1);
    fold.shells.push((1, 0, "fold output".to_owned()));
    fold.shell_index.entry((1, 0)).or_default().push(0);
    preview.carry_chain(&mut fold);
    assert_eq!(
        fold.shells,
        vec![
            (0, 0, "ancestor output".to_owned()),
            (1 + len, summaries, "fold output".to_owned()),
        ],
        "the carried shell lands first and the fold's own shifts past it"
    );
}

/// `carry_chain` carries only stale pages below the carried count, and
/// shifts the fold's own by it. Kills every `<` mutant on the stale
/// remap (`<=` keeps the boundary, `==` and `>` drop the carried one).
#[test]
fn carry_chain_carries_only_stale_below_the_carried_count() {
    use std::collections::BTreeSet;
    let (mut preview, mut fold, len, _summaries) = carry_edge();
    preview.residency.stale_mut().insert(0);
    preview.residency.stale_mut().insert(1);
    preview.residency.stale_mut().insert(len);
    fold.residency.stale_mut().insert(1);
    preview.carry_chain(&mut fold);
    assert_eq!(
        fold.residency.stale(),
        &BTreeSet::from([0, 1, 1 + len]),
        "the carried stale move over and the fold's own shifts past them"
    );
}

/// `carry_chain` carries only failed pages below the carried count, and
/// shifts the fold's own by it. Kills every `<` mutant on the failed
/// remap.
#[test]
fn carry_chain_carries_only_failed_below_the_carried_count() {
    use std::collections::BTreeSet;
    let (mut preview, mut fold, len, _summaries) = carry_edge();
    preview.fail(&SessionId(ANCESTOR.to_owned()), Seq(0));
    preview.fail(&SessionId(SESSION.to_owned()), Seq(2));
    fold.fail(&SessionId(SESSION.to_owned()), Seq(64));
    preview.carry_chain(&mut fold);
    assert_eq!(
        fold.residency.failed(),
        &BTreeSet::from([0, 1 + len]),
        "the carried failure moves over and the fold's own shifts past it"
    );
}

/// `carry_chain` carries only resident pages below the carried count,
/// and shifts the fold's own by it. The preview keeps its own closed
/// page resident exactly at the boundary, which also kills the `&&` to
/// `||` mutant: the boundary page must not join the fold's set.
/// Together the two asserts kill every `<` mutant on the resident guard
/// too (`<=` and `||` keep the boundary, `==` and `>` drop the carried
/// ones).
#[test]
fn carry_chain_carries_only_residents_below_the_carried_count() {
    use std::collections::BTreeSet;
    let (preview, mut fold, len, _summaries) = carry_edge();
    assert!(
        preview.part(len).is_some(),
        "the own page at the boundary is resident"
    );
    fold.residency.resident_set_mut().clear();
    fold.residency.resident_set_mut().insert(5);
    preview.carry_chain(&mut fold);
    assert_eq!(
        fold.residency.resident_set(),
        &BTreeSet::from([0, 1, 2, 5 + len]),
        "the carried residents move over and the fold's own shifts past them"
    );
}

/// `carry_chain` carries only pins below the carried count, and shifts
/// the fold's own by it. Kills every `<` mutant on the pins filter
/// (`<=` keeps the boundary pin, `>` drops the carried one).
#[test]
fn carry_chain_carries_only_pins_below_the_carried_count() {
    let (mut preview, mut fold, len, _summaries) = carry_edge();
    preview.want(0);
    preview.want(len);
    fold.want(1);
    preview.carry_chain(&mut fold);
    let mut pinned: Vec<usize> = fold.residency.pins().pages().collect();
    pinned.sort_unstable();
    assert_eq!(
        pinned,
        vec![0, 1 + len],
        "the carried pin moves over and the fold's own shifts past it"
    );
}

#[test]
fn copy_finds_the_newest_reply_from_the_open_page_back() {
    let lines = session(3, false);
    let pages = pages_at(&lines, 80);
    // The open page holds the last turn's last step.
    let crate::app::copy::LastReply::Text(newest) =
        crate::app::copy::last_reply(pages.newest_first())
    else {
        panic!("a reply");
    };
    assert!(newest.starts_with("reply 2_29: "), "{newest}");
    // With the open page set aside, the walk goes back through the
    // closed pages, newest first.
    let empty: &[Turn] = &[];
    let closed = std::iter::once(Some(empty)).chain(pages.newest_first().skip(1));
    let crate::app::copy::LastReply::Text(older) = crate::app::copy::last_reply(closed) else {
        panic!("a reply");
    };
    assert!(older.starts_with("reply 2_"), "{older}");
    assert_ne!(older, newest);
}

/// Types `/copy` and presses Enter.
fn copy(app: &mut App) -> Effect {
    for ch in "/copy".chars() {
        assert_eq!(app.on_key(Key::Char(ch), now()), Effect::None);
    }
    app.on_key(Key::Enter, now())
}

#[test]
fn copy_says_a_paged_out_reply_is_paged_out_until_the_end_loads_it() {
    let mut stream = Stream::new(true);
    for turn in 0..3 {
        stream.turn(turn, 30);
    }
    // Two turns with no reply, long enough that the open page holds none.
    for turn in 3..5 {
        stream.durable(
            "turn_started",
            None,
            json!({"input": [{"type": "message", "source": "driver",
                "content": [{"type": "text", "text": format!("prompt {turn}")}]}]}),
        );
        for _ in 0..80 {
            stream.durable("unknown", None, json!({}));
        }
    }
    let lines = stream.lines;
    let mut app = app(80, 20);
    open(&mut app, &lines);
    app.jump(0);
    frame(&mut app, &lines);
    assert_eq!(copy(&mut app), Effect::None);
    assert_eq!(
        app.notice(),
        Some("The last reply is paged out; jump to the end, then /copy again.")
    );
    assert!(!app.copied());
    // At the end the window holds the page with the newest reply again.
    assert_eq!(app.on_key(Key::End, now()), Effect::None);
    frame(&mut app, &lines);
    let Effect::Copy(text) = copy(&mut app) else {
        panic!("copied");
    };
    assert!(text.starts_with("reply 2_29: "), "{text}");
    assert!(app.copied());
}

#[test]
fn a_dropped_page_draw_keeps_the_frame_wall_time() {
    use crate::ext::tests::AnimatedRenderer;
    use std::time::Duration;
    let (mut pages, _lines) = work_pages(4, 80);
    drop_half(&mut pages);
    assert!(pages.part(0).is_none(), "page 0 is dropped");
    let resident = pages.part(2).cloned().expect("page 2 is resident");
    // The window binds a dropped page through a scratch cache sharing
    // the resident install, flag and wall time.
    let fake = std::rc::Rc::new(AnimatedRenderer::filling(
        "reply",
        Duration::from_millis(500),
        1,
    ));
    pages.ext.handle = crate::ext::Handle::create();
    pages
        .ext
        .handle
        .set(fake.clone() as std::rc::Rc<dyn crate::ext::Renderer>);
    pages.ext.wall_ms = Some(1_000);
    let ((rows, _, _, _), _) = pages.draw_sourced(0, &resident, super::Draw::Shown);
    assert!(!rows.is_empty(), "the dropped page draws");
    assert_eq!(fake.frames.borrow().clone(), vec![0]);
    assert_eq!(fake.nows.borrow().clone(), vec![1_000]);
}
