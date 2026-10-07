//! Tests for the window of history (`docs/tui.md`, "History and paging"):
//! pages joined draw what one fold of the whole session draws, the window
//! holds the only pages kept, loading and dropping never move a row, a new
//! width counts every page again, and what the person opened survives a
//! page's reload.

use std::ops::RangeInclusive;
use std::path::PathBuf;

use contract::clock::Clock;
use contract::{ActionId, Envelope, Seq, SessionId};
use serde_json::{Value, json};

use super::{Folded, Pages, Part, fold};
use crate::app::{App, Effect, Target};
use crate::keys::Key;
use crate::link::Line;
use crate::turn::Row;

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
            "cost": null});
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
    let mut out = Vec::new();
    pages.draw(0, &part, &mut out);
    out
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
                pages.load(&history(lines, &range));
            }
        }
        assert!(pages.part(at).is_some(), "page {at} did not load");
        let mut rows = Vec::new();
        if let Some(part) = pages.part(at) {
            pages.draw(at, part, &mut rows);
        }
        let drawn: usize = rows
            .iter()
            .map(|(line, _)| crate::view::rows(line.clone(), pages.width))
            .sum();
        let counted = pages.index().pages().get(at).map(|page| page.rows);
        assert_eq!(counted, Some(drawn), "page {at}");
        out.extend(rows);
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
    pages.closed.fill(None);
}

#[test]
fn pages_joined_draw_what_one_fold_draws() {
    for live in [true, false] {
        let lines = session(6, live);
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
    while let Some(range) = app.needs().first().cloned() {
        app.load(history(lines, &range));
        assert_ne!(app.needs().first(), Some(&range), "loading did not settle");
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
    let lines = session(6, true);
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
    let lines = session(6, true);
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
    let lines = session(6, true);
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
    assert!(texts.contains(&"still streaming".to_owned()));
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
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Copy(_) => Vec::new(),
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
    let Some(range) = range else {
        return;
    };
    app.load_failed(&range, "from_seq is past the latest line");
    assert_eq!(
        app.notice(),
        Some("Could not load history: from_seq is past the latest line")
    );
    assert!(!app.needs().contains(&range));
    frame(&mut app, &lines);
    assert!(app.pages().part(0).is_none());
    assert_eq!(layout(&app), before);
    // Its rows draw blank.
    let (first, shown) = app.shown(0, 1);
    assert_eq!(first, 0);
    assert_eq!(
        shown.first().map(|(line, _, _)| line.to_string()),
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
    let mut rows = Vec::new();
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
    let lines = session(6, false);
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
        texts.iter().any(|line| line.starts_with("  2 read")),
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

#[test]
fn dropped_pages_leave_no_rendered_text_in_their_seeds() {
    let lines = crashed_session();
    let mut pages = Pages::new(80);
    for line in &lines {
        pages.apply(line);
    }
    assert!(pages.index().pages().len() > 2, "too few pages");
    // The resident pages draw the orphan line under its job's description.
    let live: Vec<String> = pages
        .rows()
        .iter()
        .map(|(line, _)| line.to_string())
        .collect();
    assert!(
        live.iter()
            .any(|line| line.contains("zz describe the indescribable")),
        "{live:?}"
    );
    drop_all(&mut pages);
    // Every other page is dropped: its seed keeps the continuation state,
    // and no aside or job text from the dropped pages.
    let seeds = format!("{:?}", pages.seeds);
    for needle in [
        "zz describe the indescribable",
        "zz orphaned away",
        "Orphaned jobs",
        "↺ resumed",
    ] {
        assert!(!seeds.contains(needle), "a seed keeps {needle:?}");
    }
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

#[test]
fn open_job_descriptions_survive_live_page_cuts_and_reload() {
    let lines = job_completed_after_page_cut();
    let mut pages = Pages::new(80);
    for line in &lines {
        pages.apply(line);
    }
    let started = lines
        .iter()
        .find(|line| line.kind == "job_started")
        .and_then(|line| line.seq)
        .expect("job_started has a seq");
    let completed = lines
        .iter()
        .find(|line| line.kind == "job_completed")
        .and_then(|line| line.seq)
        .expect("job_completed has a seq");
    assert_ne!(
        pages.index().page_of(started),
        pages.index().page_of(completed),
        "the job events must straddle a page cut"
    );

    let live = pages.rows();
    let live_texts: Vec<String> = live.iter().map(|(line, _)| line.to_string()).collect();
    assert!(
        live_texts
            .iter()
            .any(|line| line == "Orphaned jobs: build the docs"),
        "{live_texts:?}"
    );

    drop_all(&mut pages);
    let reloaded = joined(&mut pages, &lines);
    let reloaded_texts: Vec<String> = reloaded.iter().map(|(line, _)| line.to_string()).collect();
    assert!(
        reloaded_texts
            .iter()
            .any(|line| line == "Orphaned jobs: build the docs"),
        "{reloaded_texts:?}"
    );
    assert_eq!(differs(&reloaded, &live), None);
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

/// The page holding the first line of `kind`.
fn page_of_kind(pages: &Pages, lines: &[Envelope], kind: &str) -> usize {
    lines
        .iter()
        .find(|line| line.kind == kind)
        .and_then(|line| line.seq)
        .and_then(|seq| pages.index().page_of(seq))
        .expect("the line is on a page")
}

/// The drawn lines' texts.
fn texts(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|(line, _)| line.to_string()).collect()
}

/// How many times `needle` shows in the seeds, the closed resident pages
/// and the open page's state.
fn held(pages: &Pages, needle: &str) -> (usize, usize, usize) {
    (
        format!("{:?}", pages.seeds).matches(needle).count(),
        format!("{:?}", pages.closed).matches(needle).count(),
        format!("{:?}", pages.open).matches(needle).count(),
    )
}

/// Folds `lines` live and checks the orphan line names `name` live, with
/// every page dropped and loaded again, and after a new width with every
/// page dropped; each reload draws what the live fold drew.
fn orphan_line_survives(lines: &[Envelope], name: &str) -> Pages {
    let mut pages = Pages::new(80);
    for line in lines {
        pages.apply(line);
    }
    let want = format!("Orphaned jobs: {name}");
    let live = pages.rows();
    assert!(texts(&live).contains(&want), "{:?}", texts(&live));
    drop_all(&mut pages);
    let reloaded = joined(&mut pages, lines);
    assert_eq!(differs(&reloaded, &live), None);
    drop_all(&mut pages);
    pages.set_width(50);
    let narrow = joined(&mut pages, lines);
    assert!(texts(&narrow).contains(&want), "{:?}", texts(&narrow));
    pages
}

#[test]
fn open_job_descriptions_survive_step_cuts_and_reload() {
    for pending in [false, true] {
        let lines = crossing_step(pending);
        let pages = orphan_line_survives(&lines, "step over the line");
        let started = page_of_kind(&pages, &lines, "job_started");
        let completed = page_of_kind(&pages, &lines, "job_completed");
        assert_eq!(completed, started + 1, "pending {pending}");
        assert!(pages.seeds.get(started).is_some_and(|seed| seed.cut));
        assert!(completed < pages.closed.len(), "the completion page closes");
    }
}

#[test]
fn a_confirmed_step_cut_keeps_what_its_closed_page_carried() {
    let lines = orphaned_before_step_cut();
    let pages = orphan_line_survives(&lines, "late to the cut");
    let completed = page_of_kind(&pages, &lines, "job_completed");
    assert_eq!(completed, page_of_kind(&pages, &lines, "job_started") + 1);
    assert!(pages.seeds.get(completed).is_some_and(|seed| seed.cut));
    let carried = pages.seeds.get(completed).map(|seed| seed.carried.len());
    assert_eq!(carried, Some(1));
}

#[test]
fn a_confirmed_step_cut_leaves_no_job_text_on_the_closed_page() {
    for pending in [false, true] {
        let lines = crossing_step(pending);
        let mut pages = Pages::new(80);
        for line in &lines {
            pages.apply(line);
            let started = pages.closed.first().and_then(Option::as_ref);
            if let Some(part) = started {
                let debug = format!("{part:?}");
                assert!(!debug.contains("step over the line"), "pending {pending}");
            }
        }
    }
}

#[test]
fn page_seeds_keep_no_descriptions_of_open_jobs() {
    let mut stream = Stream::new(false);
    stream.turn(0, 1);
    for index in 0..40 {
        let id = format!("j_done_{index}");
        job_started(
            &mut stream,
            &id,
            &format!("completed job {index} description"),
        );
        job_done(&mut stream, &id);
    }
    job_started(&mut stream, "j_open", "active job description");
    stream.turn(1, 1);

    let mut pages = Pages::new(80);
    for line in &stream.lines {
        pages.apply(line);
    }
    assert!(!pages.closed.is_empty(), "the job crosses a page cut");
    assert_eq!(held(&pages, "active job description"), (0, 0, 1));
    let seeds = format!("{:?}", pages.seeds);
    assert!(!seeds.contains("completed job"), "{seeds}");
}

#[test]
fn only_the_page_orphaning_a_crossing_job_carries_its_description() {
    let lines = crossing_turns(Ends::OrphanedLater);
    let pages = orphan_line_survives(&lines, "cross the boundary");
    let completed = page_of_kind(&pages, &lines, "job_completed");
    assert_ne!(page_of_kind(&pages, &lines, "job_started"), completed);
    for (at, seed) in pages.seeds.iter().enumerate() {
        let want = usize::from(at == completed);
        assert_eq!(seed.carried.len(), want, "page {at}");
    }
    assert_eq!(
        pages
            .seeds
            .get(completed)
            .and_then(|seed| seed.carried.get("j_cross"))
            .map(String::as_str),
        Some("cross the boundary")
    );
}

#[test]
fn a_crossing_job_completed_leaves_its_description_nowhere() {
    let lines = crossing_turns(Ends::DoneLater);
    let mut pages = Pages::new(80);
    for line in &lines {
        pages.apply(line);
    }
    assert_ne!(
        page_of_kind(&pages, &lines, "job_started"),
        page_of_kind(&pages, &lines, "job_completed")
    );
    assert_eq!(held(&pages, "cross the boundary"), (0, 0, 0));
}

#[test]
fn a_job_orphaned_on_its_own_page_carries_nothing() {
    let lines = crossing_turns(Ends::OrphanedHere);
    let pages = orphan_line_survives(&lines, "cross the boundary");
    assert_eq!(
        page_of_kind(&pages, &lines, "job_started"),
        page_of_kind(&pages, &lines, "job_completed")
    );
    assert!(pages.seeds.iter().all(|seed| seed.carried.is_empty()));
}

#[test]
fn open_jobs_add_no_seed_text_however_many_cuts_they_cross() {
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
        let mut pages = Pages::new(80);
        for line in &stream.lines {
            pages.apply(line);
        }
        assert_eq!(pages.closed.len(), cuts, "{cuts} cuts");
        for job in 0..5 {
            let held = held(&pages, &format!("open job {job}"));
            assert_eq!(held, (0, 0, 1), "job {job} over {cuts} cuts");
        }
    }
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
    assert!(
        texts.iter().any(|line| line.contains("400.0k")),
        "{texts:?}"
    );
    drop_all(&mut pages);
    let got = joined(&mut pages, &lines);
    assert_eq!(differs(&got, &live), None);
}

#[test]
fn reloading_keeps_the_live_durations_and_counts() {
    let mut lines = session(2, true);
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
    let (first, shown) = app.shown(top, 5);
    assert_eq!(first, top);
    assert!(!shown.is_empty());
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
    pages.load(&stream.lines);

    let part = pages.part(0).expect("reloaded first page");
    let mut rows = Vec::new();
    pages.draw(0, part, &mut rows);
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
        lines.iter().any(|line| line.starts_with("▣ cut short")),
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
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Copy(_) => panic!("expected a sent line"),
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
    assert!(!pages.wrapped.is_empty());
    // Exactly the open page's lines wider than the screen are cached, with
    // the rows drawing them takes.
    let width = 20u16;
    let at = pages.closed.len();
    let part = pages.part(at).expect("open page");
    let mut out = Vec::new();
    pages.draw(at, part, &mut out);
    let mut expected = HashMap::new();
    for (line, _) in &out {
        if line.width() > usize::from(width) {
            expected.insert(line.to_string(), crate::view::rows(line.clone(), width));
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
    pages.shells.push((0, 0, "here".to_owned()));
    pages.shells.push((1, 0, "other page".to_owned()));
    pages.shells.push((0, 1, "later turn".to_owned()));
    let part = pages.part(0).expect("open page");
    let mut out = Vec::new();
    pages.draw(0, part, &mut out);
    let texts: Vec<String> = out.iter().map(|(line, _)| line.to_string()).collect();
    assert_eq!(texts, vec!["here".to_owned()]);
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
fn toggling_ledgers_clears_only_group_overrides() {
    let mut pages = Pages::new(80);
    pages.overrides.insert(Target::Group(1), true);
    pages.overrides.insert(Target::Thought(2), false);
    pages.toggle_ledgers();
    assert!(!pages.overrides.contains_key(&Target::Group(1)));
    assert!(pages.overrides.contains_key(&Target::Thought(2)));
}
