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

use super::{Pages, Part, fold};
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
    let mut pages = Pages::new(80);
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

#[test]
fn close_keeps_the_open_page_as_a_closed_one() {
    let mut pages = Pages::new(80);
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
    let mut pages = Pages::new(80);
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
    let mut pages = Pages::new(80);
    for line in &lines {
        pages.apply(line);
    }
    // Live pages are never dropped, so the open page stands beside more
    // than one closed one: neither zero nor one matches this count.
    let total = pages.index().pages().len();
    assert!(total > 2, "{total}");
    assert_eq!(pages.resident(), total);
}
