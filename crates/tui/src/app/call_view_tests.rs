//! Tests for a call's swapped view: opening, the frame, paging with a
//! bounded window, failures, markers, `y` and scoped search
//! (`docs/tui.md`, "Swapped views").

use std::path::PathBuf;

use serde_json::{Value, json};

use super::super::{App, Effect, Target};
use crate::keys::{Key, Mouse, MouseKind};
use crate::link::Line;
use crate::mouse::TargetId;
use contract::SessionId;
use contract::clock::Clock;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const SESSION: &str = "s_aaaaaaaaaaaaaaaa";
const TURN: &str = "t_1";
const ACTION: &str = "a_1";
const ARTIFACT: &str = "artifacts/a_1.txt";
const COMMAND: &str = "cargo test -p log";

fn now() -> std::time::Instant {
    fakes::clock::FakeClock::new().now()
}

/// A connected app attached to [`SESSION`] at `width` by `height`.
fn attached(width: u16, height: u16) -> App {
    let mut app = App::new(PathBuf::from("/w"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    assert!(app.on_line(Line::Hub(hello)).is_empty());
    app.attach(SessionId(SESSION.to_owned()));
    app.set_size(width, height);
    app
}

/// One session envelope on [`SESSION`] in [`TURN`] for `action`.
fn envelope(kind: &str, payload: Value, action: Option<&str>) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: Some(contract::TurnId(TURN.to_owned())),
        action_id: action.map(|id| contract::ActionId(id.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

fn turn_started(app: &mut App) {
    app.on_line(envelope(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]}),
        None,
    ));
}

fn requested(app: &mut App, name: &str, arguments: Value, action: &str) {
    app.on_line(envelope(
        "tool_call_requested",
        json!({"name": name, "arguments": arguments}),
        Some(action),
    ));
}

fn completed(
    app: &mut App,
    action: &str,
    content: &str,
    details: Value,
    artifact: Option<&str>,
    kept: Option<Value>,
) {
    let mut payload = json!({"status": "completed",
        "content": [{"type": "text", "text": content}],
        "details": details});
    if let Some(artifact) = artifact {
        payload["artifact"] = json!(artifact);
    }
    if let Some(kept) = kept {
        payload["kept"] = kept;
    }
    app.on_line(envelope("tool_call_completed", payload, Some(action)));
}

/// A completed shell call cut at both ends, `id` for `action`.
fn shell_cut(app: &mut App, action: &str) -> usize {
    requested(app, "shell", json!({"command": COMMAND}), action);
    completed(
        app,
        action,
        "head\ntail",
        json!({"command": COMMAND, "exit_code": 0}),
        Some(ARTIFACT),
        Some(json!([[0, 5], [10, 15]])),
    );
    crate::turn::target_id(action)
}

/// A completed shell call with no cut: no artifact, no markers.
fn shell_whole(app: &mut App, action: &str) -> usize {
    requested(app, "shell", json!({"command": COMMAND}), action);
    completed(
        app,
        action,
        "one\ntwo",
        json!({"command": COMMAND, "exit_code": 0}),
        None,
        None,
    );
    crate::turn::target_id(action)
}

/// Opens `id` inline, as a click on its ledger row does.
fn open_inline(app: &mut App, id: usize) {
    app.open(Target::Call(id));
    assert!(app.call_open(id));
}

/// The command lines an effect sends, parsed.
fn sent(effect: Effect) -> Vec<Value> {
    match effect {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect(),
        Effect::None => Vec::new(),
        Effect::Quit
        | Effect::Exit(_)
        | Effect::ListFiles
        | Effect::Search { .. }
        | Effect::Editor { .. }
        | Effect::Copy(_)
        | Effect::FindPause { .. }
        | Effect::OpenLink(_)
        | Effect::ReadImage(_)
        | Effect::Feedback { .. }
        | Effect::OpenFile(_) => panic!("no lines in {effect:?}"),
    }
}

/// The single `artifact_page` line an effect sends.
fn page_request(effect: Effect) -> Value {
    let lines = sent(effect);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["command"], json!("artifact_page"));
    line.clone()
}

/// A `command_accepted` envelope answering `request` with an
/// `artifact_page` result.
fn accepted(request: &Value, text: &str, lines: Vec<u64>, next: Option<u64>, size: u64) -> Line {
    let mut result = json!({"text": text, "lines": lines, "size": size});
    if let Some(next) = next {
        result["next"] = json!(next);
    }
    envelope(
        "command_accepted",
        json!({"command_id": request["id"], "result": result}),
        None,
    )
}

/// A `command_rejected` envelope refusing `request` with `code`.
fn rejected(request: &Value, code: &str) -> Line {
    envelope(
        "command_rejected",
        json!({"command_id": request["id"], "code": code,
            "message": format!("page failed: {code}")}),
        None,
    )
}

/// The view's frame rows as text.
fn frame_text(app: &App) -> Vec<String> {
    let Some(frame) = app.call_view().map(|view| view.frame()) else {
        panic!("no call view open");
    };
    frame
        .rows
        .into_iter()
        .map(|cells| {
            cells
                .into_iter()
                .map(|(text, _, _)| text)
                .collect::<String>()
        })
        .collect()
}

/// The screen's text at `width` by `height`.
fn screen(app: &App, width: u16, height: u16) -> String {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    crate::view::render(app, area, &mut buffer, None);
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn enter_on_an_opened_call_opens_its_view_and_esc_returns_focus() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    app.focus = Some(TargetId::Line(Target::Call(id)));
    let request = page_request(app.on_key(Key::Enter, now()));
    assert!(app.call_view_open());
    assert_eq!(request["args"]["offset"], json!(0));
    assert_eq!(request["args"]["path"], json!(ARTIFACT));
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.call_view_open());
    assert_eq!(app.focused(), Some(TargetId::Line(Target::Call(id))));
}

#[test]
fn esc_keeps_the_call_row_focused_so_enter_reopens() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let _ = page_to_end_from_zero(&mut app, first, &text);
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.call_view_open());
    // The open ledger group draws the headline row, so the frame keeps
    // the Esc focus; a collapsed group would clear it. The six kept
    // lines fit the inline cap, so no more row draws: the headline is
    // the only drawn row that survives.
    let area = Rect::new(0, 0, 80, 24);
    let mut buffer = Buffer::empty(area);
    let targets = crate::view::render(&app, area, &mut buffer, None);
    app.drawn(&targets);
    assert_eq!(app.focused(), Some(TargetId::Line(Target::Call(id))));
    // Enter on the focused, still-open call opens its view again from
    // offset zero.
    let request = page_request(app.on_key(Key::Enter, now()));
    assert!(app.call_view_open());
    assert_eq!(request["args"]["offset"], json!(0));
}

#[test]
fn enter_on_a_closed_call_toggles_it_open_instead() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    app.focus = Some(TargetId::Line(Target::Call(id)));
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    assert!(!app.call_view_open());
    assert!(app.call_open(id));
}

#[test]
fn a_click_on_the_more_row_opens_the_view() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let effect = app.on_click(TargetId::Line(Target::CallView(id)));
    let request = page_request(effect);
    assert!(app.call_view_open());
    assert_eq!(request["args"]["offset"], json!(0));
}

#[test]
fn a_click_on_the_opened_call_row_keeps_the_inline_toggle() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    assert_eq!(app.on_click(TargetId::Line(Target::Call(id))), Effect::None);
    assert!(!app.call_view_open());
    assert!(!app.call_open(id));
}

#[test]
fn the_frame_names_the_kind_and_target_with_a_copy_footer() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    let shown = screen(&app, 80, 24);
    let top = shown.lines().next().unwrap_or_default().to_owned();
    assert!(top.contains("Shell"), "{top:?}");
    assert!(top.contains(COMMAND), "{top:?}");
    assert!(top.contains("Esc returns"), "{top:?}");
    assert!(shown.contains("y copies the whole call"), "{shown:?}");
    // The whole call draws with no 20-line cap: the shell's command
    // row and its closing draw around the pages.
    let rows = frame_text(&app);
    assert!(
        rows.iter().any(|row| row.contains("$ cargo test -p log")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|row| row.contains("exit 0")), "{rows:?}");
    // Top-row style: the kind bold, the target dim.
    use ratatui::style::Modifier;
    let area = Rect::new(0, 0, 80, 24);
    let mut buffer = Buffer::empty(area);
    crate::view::render(&app, area, &mut buffer, None);
    let kind_at = u16::try_from(top.find("Shell").unwrap_or(usize::MAX)).unwrap_or(u16::MAX);
    assert!(
        buffer[(kind_at, 0)].modifier.contains(Modifier::BOLD),
        "the kind draws bold"
    );
    let target_at = u16::try_from(top.find(COMMAND).unwrap_or(usize::MAX)).unwrap_or(u16::MAX);
    assert!(
        buffer[(target_at, 0)].modifier.contains(Modifier::DIM),
        "the target draws dim"
    );
    let _ = request;
}

/// A 150-line file in three pages: 64, 64 and 22 lines of `L{i:03}`.
fn lined_file() -> String {
    (0..150).map(|i| format!("L{i:03}\n")).collect::<String>()
}

/// Opens the view on a cut shell call with `kept`, feeding
/// `turn_started` first. Returns the call id and the first request.
fn open_paged(app: &mut App, kept: Value, action: &str) -> (usize, Value) {
    turn_started(app);
    requested(app, "shell", json!({"command": COMMAND}), action);
    completed(
        app,
        action,
        "cut",
        json!({"command": COMMAND, "exit_code": 0}),
        Some(ARTIFACT),
        Some(kept),
    );
    let id = crate::turn::target_id(action);
    // The wired path opens the ledger group before the call, or the
    // conversation renders the collapsed group row and no call row draws.
    app.open(Target::Group(id));
    open_inline(app, id);
    let request = page_request(app.open_call_view(id));
    (id, request)
}

/// Answers `request` with the page of `text` at `offset`: at most 64
/// whole lines, and returns the follow-up sends.
fn answer_page(app: &mut App, request: &Value, text: &str, offset: u64) -> Vec<Value> {
    let size = text.len() as u64;
    let start = usize::try_from(offset).expect("test offsets fit");
    let mut lines = Vec::new();
    let mut edge = offset;
    for line in text[start..].split_inclusive('\n').take(64) {
        lines.push(edge);
        edge += line.len() as u64;
    }
    let end = usize::try_from(edge).expect("test offsets fit");
    let chunk = &text[start..end];
    let next = (edge < size).then_some(edge);
    let sends = app.on_line(accepted(request, chunk, lines, next, size));
    sends
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect()
}

/// The next offset paging `text` announces after `offset`: `None`
/// at the end of the file.
fn page_next(text: &str, offset: u64) -> Option<u64> {
    let start = usize::try_from(offset).expect("test offsets fit");
    let mut edge = offset;
    for line in text[start..].split_inclusive('\n').take(64) {
        edge += line.len() as u64;
    }
    (edge < text.len() as u64).then_some(edge)
}

/// The requested offsets paging `text`: every follow-up asks for the
/// announced next.
fn requested_offsets(text: &str, offsets: &[u64]) {
    for pair in offsets.windows(2) {
        assert_eq!(
            Some(pair[1]),
            page_next(text, pair[0]),
            "each page asks for the announced next: {offsets:?}"
        );
    }
}

/// The requested offsets paging `text` from 0: the first request
/// starts at 0 and every follow-up asks for the announced next.
fn requested_offsets_from_zero(text: &str, offsets: &[u64]) {
    assert_eq!(offsets.first(), Some(&0), "{offsets:?}");
    requested_offsets(text, offsets);
}

/// Pages the view to the end of `text`, answering every request in
/// order, and returns the requested offsets.
fn page_to_end(app: &mut App, first: Value, text: &str) -> Vec<u64> {
    let mut offsets = vec![first["args"]["offset"].as_u64().unwrap()];
    let mut request = first;
    for _ in 0..10 {
        let at = offsets.last().copied().unwrap();
        let sends = answer_page(app, &request, text, at);
        if sends.is_empty() {
            break;
        }
        // One request on the wire: the follow-up asks for the page's
        // announced next offset, whatever else the pump sends.
        let next = page_next(text, at).expect("a mid-file page names next");
        let follow = sends
            .iter()
            .find(|line| line["args"]["offset"] == json!(next))
            .unwrap_or_else(|| panic!("the follow-up asks for {next}: {sends:?}"))
            .clone();
        offsets.push(follow["args"]["offset"].as_u64().unwrap());
        request = follow;
    }
    requested_offsets(text, &offsets);
    offsets
}

/// Pages the view to the end of `text` from a fresh open, answering
/// every request in order: the first request starts at 0.
fn page_to_end_from_zero(app: &mut App, first: Value, text: &str) -> Vec<u64> {
    let offsets = page_to_end(app, first, text);
    requested_offsets_from_zero(text, &offsets);
    offsets
}

#[test]
fn pages_land_in_order_and_evict_then_refetch_by_offset() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let offsets = page_to_end_from_zero(&mut app, first, &text);
    // A 150-line file needs more than two 64-line pages.
    assert!(offsets.len() >= 3, "{offsets:?}");
    // Following pulled to the bottom: the window holds the last pages,
    // and the first paged out.
    let rows = frame_text(&app);
    assert!(rows.iter().any(|row| row.contains("L149")), "{rows:?}");
    assert!(!rows.iter().any(|row| row.contains("L000")), "{rows:?}");
    // Scrolling back to the top refetches the dropped page by its byte
    // offset.
    let mut refetched = None;
    for _ in 0..30 {
        let effect = app.on_key(Key::PageUp, now());
        for line in sent(effect) {
            if line["command"] == json!("artifact_page") {
                refetched = Some(line["args"]["offset"].as_u64().unwrap());
            }
        }
        if refetched.is_some() {
            break;
        }
    }
    assert_eq!(refetched, Some(0), "the dropped first page refetches");
    let view = app.call_view().expect("the view stays open");
    assert!(
        view.pager.segs.contains_key(&0),
        "extents survive paging out"
    );
}

#[test]
fn following_many_pages_keeps_the_tail_visible_after_page_out() {
    let mut app = attached(80, 24);
    let text = (0..500).map(|i| format!("L{i:03}\n")).collect::<String>();
    let size = text.len() as u64;
    let (_id, first) = open_paged(&mut app, json!([[0, 5], [size - 5, size]]), ACTION);
    let offsets = page_to_end_from_zero(&mut app, first, &text);
    assert!(offsets.len() > 3, "the test crosses the held-page window");
    let view = app.call_view().expect("the view stays open");
    let rows = view.rows();
    assert!(view.list.top() < rows.len(), "the first shown row exists");
    assert!(view.list.selected() < rows.len(), "the selected row exists");
    let shown = rows
        .iter()
        .skip(view.list.top())
        .take(crate::swapped::rows_height(&view.frame(), 24));
    let shown = shown
        .flat_map(|row| row.cells.iter())
        .map(|(text, _)| text.as_str())
        .collect::<String>();
    assert!(shown.contains("L499"), "the tail stays visible: {shown:?}");
    assert!(
        shown.contains("the model's view resumes"),
        "the resume marker stays visible: {shown:?}"
    );
}

#[test]
fn a_failed_page_keeps_held_pages_and_retries_on_scroll() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let sends = answer_page(&mut app, &first, &text, 0);
    // The follow-up asks for the first page's announced next offset.
    let next = page_next(&text, 0).expect("the first page names next");
    let second = sends
        .iter()
        .find(|line| line["args"]["offset"] == json!(next))
        .unwrap_or_else(|| panic!("the follow-up asks for {next}: {sends:?}"))
        .clone();
    // The second page fails: the view stays open with the first page
    // and a notice naming the error code.
    assert!(app.on_line(rejected(&second, "io_failed")).is_empty());
    let rows = frame_text(&app);
    assert!(rows.iter().any(|row| row.contains("L000")), "{rows:?}");
    let frame = app.call_view().map(|view| view.frame()).expect("open");
    assert_eq!(frame.below.len(), 1, "{frame:?}");
    assert!(frame.below[0].contains("io_failed"), "{frame:?}");
    // A `y` copy waits on no partial text: nothing writes, and the
    // error-code notice stands.
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::None);
    assert_eq!(app.take_call_copy(), None);
    assert!(!app.copied());
    // Scrolling the missing page into the window asks for it again.
    let mut retried = None;
    for _ in 0..30 {
        let effect = app.on_key(Key::PageDown, now());
        for line in sent(effect) {
            if line["command"] == json!("artifact_page") && line["args"]["offset"] == json!(320) {
                retried = Some(line.clone());
            }
        }
        if retried.is_some() {
            break;
        }
    }
    let retried = retried.expect("the missing page re-requests on scroll");
    // Landing it fills the gap and clears the notice; the pump follows
    // the page's announced next offset.
    let sends = answer_page(&mut app, &retried, &text, 320);
    let next = page_next(&text, 320).expect("a mid-file page names next");
    assert!(
        sends
            .iter()
            .any(|line| line["args"]["offset"] == json!(next)),
        "the follow-up asks for {next}: {sends:?}"
    );
    let frame = app.call_view().map(|view| view.frame()).expect("open");
    assert!(frame.below.is_empty(), "{frame:?}");
    let rows = frame_text(&app);
    assert!(rows.iter().any(|row| row.contains("L064")), "{rows:?}");
}

/// The body rows' texts of an opened view, markers included.
fn body_rows(app: &App) -> Vec<String> {
    frame_text(app)
}

/// A both-ends cut call over `text` with `kept`, answered whole in one
/// page, returning the view's rows.
fn cut_rows(text: &str, kept: Value) -> Vec<String> {
    let mut app = attached(80, 24);
    let action = "a_9";
    turn_started(&mut app);
    requested(&mut app, "shell", json!({"command": COMMAND}), action);
    completed(
        &mut app,
        action,
        "cut",
        json!({"command": COMMAND, "exit_code": 0}),
        Some(ARTIFACT),
        Some(kept),
    );
    let id = crate::turn::target_id(action);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    let size = text.len() as u64;
    let mut lines = Vec::new();
    let mut at = 0u64;
    for line in text.split_inclusive('\n') {
        lines.push(at);
        at += line.len() as u64;
    }
    assert!(
        app.on_line(accepted(&request, text, lines, None, size))
            .is_empty()
    );
    body_rows(&app)
}

const END: &str = "── end of the model's view ──";
const RESUME: &str = "── the model's view resumes ──";

#[test]
fn markers_sit_at_the_first_line_boundary_past_each_edge() {
    // "ab\n" is [0, 3), "cdefgh\n" is [3, 10), "ij\n" is [10, 13).
    let rows = cut_rows("ab\ncdefgh\nij\n", json!([[0, 2], [8, 10]]));
    // The model's view ends inside "ab": the end marker goes after the
    // line that contains byte 2, before "cdefgh". Both later edges land
    // on the start of "ij", so the resume and end markers draw there in
    // edge order.
    assert_eq!(
        rows,
        vec![
            "$ cargo test -p log",
            "ab",
            END,
            "cdefgh",
            RESUME,
            END,
            "ij",
            "exit 0",
        ],
        "{rows:?}"
    );
}

#[test]
fn a_marker_edge_at_below_and_above_a_line_start() {
    // Starts are [0, 3, 10): edges 2 and 3 anchor before "cdefgh",
    // while edge 4 anchors before "ij".
    for (edge, after) in [(2, "cdefgh"), (3, "cdefgh"), (4, "ij")] {
        let rows = cut_rows("ab\ncdefgh\nij\n", json!([[0, edge]]));
        let end = rows.iter().position(|row| row == END).expect("end marker");
        assert_eq!(rows[end + 1], after, "edge {edge}");
    }
}

#[test]
fn a_marker_edge_past_the_last_start_waits_for_the_end() {
    // Bytes "a\nbcd" hold starts [0, 2]: an edge at 3 has no later
    // start until the file ends, when the marker goes after the final
    // fragment, before the closing row.
    let rows = cut_rows("a\nbcd", json!([[0, 3]]));
    let at = rows.iter().position(|row| row == "bcd").expect("last line");
    assert_eq!(rows[at + 1], END, "{rows:?}");
}

#[test]
fn malformed_bytes_map_the_edge_to_raw_offsets() {
    // b"\xffA\nB\n" pages lines [0, 3] with text "�A\nB\n": an edge at
    // byte 4 lands after "B", never shifted by the replacement char.
    let mut app = attached(80, 24);
    let action = "a_9";
    turn_started(&mut app);
    requested(&mut app, "shell", json!({"command": COMMAND}), action);
    completed(
        &mut app,
        action,
        "cut",
        json!({"command": COMMAND, "exit_code": 0}),
        Some(ARTIFACT),
        Some(json!([[0, 4]])),
    );
    let id = crate::turn::target_id(action);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    assert!(
        app.on_line(accepted(&request, "\u{fffd}A\nB\n", vec![0, 3], None, 5))
            .is_empty()
    );
    let rows = body_rows(&app);
    let end = rows.iter().position(|row| row == END).expect("end marker");
    assert_eq!(rows[end - 1], "B", "{rows:?}");
}

#[test]
fn a_continuation_start_is_no_boundary() {
    // A 100-byte first line with no newline, then "C\nDE\n" from byte
    // 100: the second page's first start equals its offset but
    // continues the long line, so an edge at 103 lands after "DE".
    let text = format!("{}C\nDE\n", "x".repeat(100));
    let mut app = attached(80, 24);
    let action = "a_9";
    turn_started(&mut app);
    requested(&mut app, "shell", json!({"command": COMMAND}), action);
    completed(
        &mut app,
        action,
        "cut",
        json!({"command": COMMAND, "exit_code": 0}),
        Some(ARTIFACT),
        Some(json!([[0, 103]])),
    );
    let id = crate::turn::target_id(action);
    open_inline(&mut app, id);
    let size = text.len() as u64;
    let first = page_request(app.open_call_view(id));
    let sends: Vec<Value> = app
        .on_line(accepted(&first, &"x".repeat(100), vec![0], Some(100), size))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    // The follow-up asks for the head page's announced next offset.
    let second = sends
        .iter()
        .find(|line| line["args"]["offset"] == json!(100))
        .expect("the follow-up asks for 100: {sends:?}")
        .clone();
    assert!(
        app.on_line(accepted(&second, "C\nDE\n", vec![100, 102], None, size))
            .is_empty()
    );
    let rows = body_rows(&app);
    // No marker before "C": the 100 start continues the long line. The
    // edge lands after the final fragment instead.
    assert!(!rows.iter().any(|row| row == RESUME), "{rows:?}");
    let end = rows.iter().position(|row| row == END).expect("end marker");
    assert_eq!(rows[end - 1], "DE");
}

#[test]
fn a_marker_skips_a_byte_cap_continuation_start() {
    // The long line crosses the byte cap: page 1 holds [0] with no
    // newline, page 2 holds [65536, 70001]. An edge at 2048, inside the
    // long line, ignores the 65536 continuation start and lands before
    // line 70001.
    let mut app = attached(80, 24);
    let action = "a_9";
    turn_started(&mut app);
    requested(&mut app, "shell", json!({"command": COMMAND}), action);
    completed(
        &mut app,
        action,
        "cut",
        json!({"command": COMMAND, "exit_code": 0}),
        Some(ARTIFACT),
        Some(json!([[0, 2048]])),
    );
    let id = crate::turn::target_id(action);
    open_inline(&mut app, id);
    let first = page_request(app.open_call_view(id));
    let head = "x".repeat(65536);
    let sends: Vec<Value> = app
        .on_line(accepted(&first, &head, vec![0], Some(65536), 70006))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    // The follow-up asks for the head page's announced next offset.
    let second = sends
        .iter()
        .find(|line| line["args"]["offset"] == json!(65536))
        .expect("the follow-up asks for 65536: {sends:?}")
        .clone();
    assert!(
        app.on_line(accepted(
            &second,
            "yyy\nzzz\n",
            vec![65536, 70001],
            None,
            70006
        ))
        .is_empty()
    );
    let rows = body_rows(&app);
    let end = rows.iter().position(|row| row == END).expect("end marker");
    assert_eq!(rows[end + 1], "zzz", "{rows:?}");
}

#[test]
fn a_one_end_cut_draws_its_single_marker() {
    // The head was cut: only the resume marker draws, before "ij", the
    // first line starting at or past byte 5.
    let rows = cut_rows("ab\ncdefgh\nij\n", json!([[5, 13]]));
    assert!(!rows.iter().any(|row| row == END), "{rows:?}");
    let resume = rows.iter().position(|row| row == RESUME).expect("resume");
    assert_eq!(rows[resume + 1], "ij");
}

#[test]
fn no_markers_without_kept_or_artifact() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    requested(&mut app, "shell", json!({"command": COMMAND}), ACTION);
    completed(
        &mut app,
        ACTION,
        "whole",
        json!({"command": COMMAND, "exit_code": 0}),
        Some(ARTIFACT),
        None,
    );
    let id = crate::turn::target_id(ACTION);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    assert!(
        app.on_line(accepted(&request, "whole\n", vec![0], None, 6))
            .is_empty()
    );
    let rows = body_rows(&app);
    assert!(
        !rows.iter().any(|row| row == END || row == RESUME),
        "{rows:?}"
    );
}

#[test]
fn y_copies_a_loaded_view_at_once() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    let text = "head\ntail\n";
    assert!(
        app.on_line(accepted(&request, text, vec![0, 5], None, 10))
            .is_empty()
    );
    assert_eq!(
        app.on_key(Key::Char('y'), now()),
        Effect::Copy(text.to_owned())
    );
    assert!(app.copied());
}

#[test]
fn y_fetches_unloaded_pages_then_copies_whole() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    // Pressing `y` with nothing held fetches one page at a time and
    // copies whole once every page lands.
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::None);
    let mut request = first;
    for _ in 0..10 {
        let offset = request["args"]["offset"].as_u64().unwrap();
        let sends = answer_page(&mut app, &request, &text, offset);
        if sends.is_empty() {
            break;
        }
        // One request on the wire: the follow-up asks for the page's
        // announced next offset, whatever else the pump sends.
        let next = page_next(&text, offset).expect("a mid-file page names next");
        request = sends
            .iter()
            .find(|line| line["args"]["offset"] == json!(next))
            .unwrap_or_else(|| panic!("the follow-up asks for {next}: {sends:?}"))
            .clone();
    }
    assert_eq!(app.take_call_copy(), Some(text.clone()));
    assert!(app.copied());
    // The copy reapplies the bounded window: the head page evicts.
    let view = app.call_view().expect("the view stays open");
    assert!(
        !view.pager.pages.contains_key(&0),
        "the window drops the head page"
    );
    assert!(
        view.pager.pages.len() <= 3,
        "the window bounds the held pages"
    );
}

#[test]
fn a_running_copy_holds_pages_past_the_window() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    // Page to the end, so the head page evicts out of the window.
    page_to_end_from_zero(&mut app, first, &text);
    assert!(!app.call_view().expect("open").pager.pages.contains_key(&0));
    // Pressing `y` with the head page missing starts the copy and
    // fetches it without copying yet.
    let sends = sent(app.on_key(Key::Char('y'), now()));
    let head = sends
        .iter()
        .find(|line| line["args"]["offset"] == json!(0))
        .expect("the copy refetches the head page: {sends:?}")
        .clone();
    // Answering lands the head page with nothing left to fetch.
    assert!(answer_page(&mut app, &head, &text, 0).is_empty());
    // The landed head page stays while the copy runs, though the
    // window shows the tail: nothing evicts mid-copy.
    let view = app.call_view().expect("the view stays open");
    assert_eq!(view.pager.pages.len(), 3, "the copy holds every page");
    // The copy lands whole, then the bounded window applies again.
    assert_eq!(app.take_call_copy(), Some(text.clone()));
    let view = app.call_view().expect("the view stays open");
    assert!(
        !view.pager.pages.contains_key(&0),
        "the window drops the head page"
    );
}

#[test]
fn y_while_a_page_is_in_flight_waits_for_the_rest_in_order() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    // The first page is on the wire when `y` is pressed: the copy waits
    // for it plus the rest, in order.
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::None);
    let offsets = page_to_end_from_zero(&mut app, first, &text);
    // A 150-line file needs more than two 64-line pages.
    assert!(offsets.len() >= 3, "{offsets:?}");
    assert_eq!(app.take_call_copy(), Some(text.clone()));
    let view = app.call_view().expect("the view stays open");
    assert!(
        !view.pager.pages.contains_key(&0),
        "the window drops the head page"
    );
}

#[test]
fn a_failed_copy_writes_nothing_and_retries_after_scroll() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let sends = answer_page(&mut app, &first, &text, 0);
    // The second page is the first page's announced next offset.
    let next = page_next(&text, 0).expect("the first page names next");
    let second = sends
        .iter()
        .find(|line| line["args"]["offset"] == json!(next))
        .unwrap_or_else(|| panic!("the follow-up asks for {next}: {sends:?}"))
        .clone();
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::None);
    // The second page fails mid-copy: no partial text writes, the
    // clipboard keeps its value, and the error-code notice shows.
    assert!(app.on_line(rejected(&second, "io_failed")).is_empty());
    assert_eq!(app.take_call_copy(), None);
    assert!(!app.copied());
    // The abort reapplies the bounded window: the held page stays,
    // since the window shows it.
    let view = app.call_view().expect("open");
    assert!(view.pager.pages.contains_key(&0), "the held page stays");
    let frame = app.call_view().map(|view| view.frame()).expect("open");
    assert!(
        frame.below.iter().any(|line| line.contains("io_failed")),
        "{frame:?}"
    );
    // Scrolling the missing page back in retries it; then `y` copies
    // the whole text.
    let mut retried = None;
    for _ in 0..30 {
        let effect = app.on_key(Key::PageDown, now());
        for line in sent(effect) {
            if line["command"] == json!("artifact_page") && line["args"]["offset"] == json!(320) {
                retried = Some(line.clone());
            }
        }
        if retried.is_some() {
            break;
        }
    }
    let retried = retried.expect("the missing page re-requests on scroll");
    page_to_end(&mut app, retried, &text);
    // Following paged the first page out again, so `y` refetches it and
    // then copies the whole text.
    let sends = sent(app.on_key(Key::Char('y'), now()));
    let head = sends
        .iter()
        .find(|line| line["args"]["offset"] == json!(0))
        .expect("the copy refetches the head page: {sends:?}")
        .clone();
    assert!(answer_page(&mut app, &head, &text, 0).is_empty());
    assert_eq!(app.take_call_copy(), Some(text.clone()));
    assert!(app.copied());
}

/// Types `query` into the find bar while the view is open, running the
/// pause expiry: no conversation page reads while the view is open.
fn type_query(app: &mut App, query: &str) {
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    assert!(app.find_open());
    let mut generation = 0;
    for ch in query.chars() {
        match app.on_key(Key::Char(ch), now()) {
            Effect::FindPause {
                generation: gen_no, ..
            } => generation = gen_no,
            Effect::None
            | Effect::Send(_)
            | Effect::Quit
            | Effect::Exit(_)
            | Effect::ListFiles
            | Effect::Search { .. }
            | Effect::Editor { .. }
            | Effect::Copy(_)
            | Effect::OpenLink(_)
            | Effect::ReadImage(_)
            | Effect::Feedback { .. }
            | Effect::OpenFile(_) => panic!("typing queries pauses"),
        }
    }
    let sends = app.find_due(generation);
    assert!(
        sends.is_empty(),
        "no conversation reads while open: {sends:?}"
    );
}

/// The view's current match row, if any.
fn current_row(app: &App) -> Option<usize> {
    let view = app.call_view().expect("the view stays open");
    view.current.and_then(|at| view.matches.get(at).copied())
}

#[test]
fn search_matches_inside_the_view_only() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    assert!(
        app.on_line(accepted(
            &request,
            "head\ncargo tail\n",
            vec![0, 5],
            None,
            16
        ))
        .is_empty()
    );
    type_query(&mut app, "cargo");
    let view = app.call_view().expect("the view stays open");
    assert!(!view.matches.is_empty(), "body rows match");
    // The bar counts the view's matches.
    let bar = app.find_bar().expect("the bar stays open");
    assert_eq!(bar.query, "cargo");
    assert!(
        bar.count.contains(&view.matches.len().to_string()),
        "{bar:?}"
    );
    // Enter moves the current match between view matches.
    let first = current_row(&app).expect("a current match");
    assert_eq!(app.on_key(Key::Enter, now()), Effect::None);
    let second = current_row(&app).expect("a current match still");
    assert_ne!(first, second, "Enter moves the match");
    // Wrapping at either end stays inside the view.
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(current_row(&app), Some(first));
    // Chrome never matches: the marker text, the title and the footer.
    for query in ["the model's view", "shell", "copies the whole"] {
        type_query(&mut app, query);
        let view = app.call_view().expect("the view stays open");
        assert!(view.matches.is_empty(), "{query} matches chrome");
        assert_eq!(
            app.find_bar().map(|bar| bar.count),
            Some("no matches".to_owned())
        );
    }
}

#[test]
fn closing_the_view_restores_conversation_search() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    // The conversation holds "zebra stripes"; the view holds none of it.
    app.on_line(envelope(
        "assistant_message_delta",
        json!({"text": "zebra stripes"}),
        Some("m_1"),
    ));
    app.on_line(envelope(
        "text_completed",
        json!({"text": "zebra stripes"}),
        Some("m_1"),
    ));
    app.on_line(envelope(
        "assistant_message_completed",
        json!({"outcome": "completed"}),
        Some("m_1"),
    ));
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    assert!(
        app.on_line(accepted(&request, "head\ntail\n", vec![0, 5], None, 10))
            .is_empty()
    );
    type_query(&mut app, "zebra");
    assert!(app.call_view().expect("open").matches.is_empty());
    assert_eq!(
        app.find_bar().map(|bar| bar.count),
        Some("no matches".to_owned())
    );
    // Closing the bar, then the view, restores the conversation's scan.
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.find_open());
    assert_eq!(app.on_key(Key::Esc, now()), Effect::None);
    assert!(!app.call_view_open());
    assert_eq!(app.focused(), Some(TargetId::Line(Target::Call(id))));
    assert_eq!(app.on_key(Key::CtrlF, now()), Effect::None);
    type_query(&mut app, "zebra");
    app.settle_find();
    let bar = app.find_bar().expect("the bar stays open");
    assert_eq!(bar.count, "1 of 1", "{bar:?}");
}

/// Conversation marks stay off while the view is open: the frame
/// paints the view's own scoped marks, so the hidden conversation
/// rows would land on wrong cells. A search from before the view
/// opened keeps its count, but its marks wait for the view to close.
#[test]
fn conversation_marks_stay_off_while_the_view_is_open() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    // The conversation holds "zebra stripes"; the view holds none of it.
    app.on_line(envelope(
        "assistant_message_delta",
        json!({"text": "zebra stripes"}),
        Some("m_1"),
    ));
    app.on_line(envelope(
        "text_completed",
        json!({"text": "zebra stripes"}),
        Some("m_1"),
    ));
    app.on_line(envelope(
        "assistant_message_completed",
        json!({"outcome": "completed"}),
        Some("m_1"),
    ));
    let id = shell_whole(&mut app, ACTION);
    open_inline(&mut app, id);
    // A conversation search first: the bar stays open with its hit.
    type_query(&mut app, "zebra");
    app.settle_find();
    assert!(
        !app.find_marks(Rect::new(0, 0, 80, 24)).is_empty(),
        "the conversation marks before the view opens"
    );
    // The view opens under the open bar, keeping its query and count.
    assert_eq!(app.open_call_view(id), Effect::None);
    assert!(app.find_bar().is_some());
    assert!(
        app.find_marks(Rect::new(0, 0, 80, 24)).is_empty(),
        "no conversation marks over the view"
    );
}

#[test]
fn opening_needs_the_call_on_the_attached_screen() {
    // No session, no call: nothing opens and nothing sends.
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(80, 24);
    assert_eq!(app.open_call_view(1), Effect::None);
    assert!(!app.call_view_open());
    // An unknown id on the attached screen opens nothing either.
    let mut app = attached(80, 24);
    turn_started(&mut app);
    shell_whole(&mut app, ACTION);
    assert_eq!(app.open_call_view(usize::MAX), Effect::None);
    assert!(!app.call_view_open());
}

#[test]
fn the_current_match_draws_brighter_than_the_rest() {
    use crate::swapped::Ink;
    use crate::theme::Role;
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    assert!(
        app.on_line(accepted(
            &request,
            "head\ncargo tail\n",
            vec![0, 5],
            None,
            16
        ))
        .is_empty()
    );
    type_query(&mut app, "cargo");
    let view = app.call_view().expect("the view stays open");
    let frame = view.frame();
    let hits: Vec<&Ink> = frame
        .rows
        .iter()
        .flatten()
        .map(|(_, _, ink)| ink)
        .filter(|ink| matches!(ink, Ink::Marked(_)))
        .collect();
    assert!(!hits.is_empty(), "matches draw marked");
    assert!(
        hits.iter()
            .any(|ink| **ink == Ink::Marked(Role::MatchCurrent)),
        "the current match draws brightest: {hits:?}"
    );
}

#[test]
fn a_result_with_no_artifact_shows_its_rows_with_no_fetch() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_whole(&mut app, ACTION);
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    assert!(app.call_view_open());
    let rows = frame_text(&app);
    assert!(rows.iter().any(|row| row.contains("one")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("two")), "{rows:?}");
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("end of the model's view")),
        "{rows:?}"
    );
}

#[test]
fn y_copies_raw_output_with_no_framing_for_a_result_with_no_artifact() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_whole(&mut app, ACTION);
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    // The draw keeps the shell's command and closing rows, but `y`
    // copies the output as the tool returned it.
    let rows = frame_text(&app);
    assert!(rows.iter().any(|row| row.contains("$ ")), "{rows:?}");
    assert_eq!(
        app.on_key(Key::Char('y'), now()),
        Effect::Copy("one\ntwo".to_owned())
    );
    assert!(app.copied());
}

#[test]
fn an_uncut_result_shows_every_line_past_twenty() {
    // A 30-line uncut result draws whole: no 20-line cap, no paging.
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let text = (0..30)
        .map(|i| format!("row{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    requested(&mut app, "shell", json!({"command": COMMAND}), ACTION);
    completed(
        &mut app,
        ACTION,
        &text,
        json!({"command": COMMAND, "exit_code": 0}),
        None,
        None,
    );
    let id = crate::turn::target_id(ACTION);
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    let rows = frame_text(&app);
    for i in 0..30 {
        assert!(
            rows.iter().any(|row| row.contains(&format!("row{i:02}"))),
            "{rows:?}"
        );
    }
}

#[test]
fn a_view_opened_on_a_running_call_refreshes_when_its_result_arrives() {
    // The body is the call drawn as inline with no 20-line cap: a view
    // opened while the call runs shows its empty rows, then the
    // completion rebuilds them and pages the new artifact.
    let mut app = attached(80, 24);
    turn_started(&mut app);
    requested(&mut app, "shell", json!({"command": COMMAND}), ACTION);
    let id = crate::turn::target_id(ACTION);
    app.open(Target::Group(id));
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    assert!(app.call_view_open());
    assert!(
        frame_text(&app).is_empty() || !frame_text(&app).iter().any(|row| row.contains("head")),
        "a running call shows no output yet: {:?}",
        frame_text(&app)
    );
    let sends: Vec<Value> = app
        .on_line(envelope(
            "tool_call_completed",
            json!({"status": "completed",
                "content": [{"type": "text", "text": "head\ntail"}],
                "details": {"command": COMMAND, "exit_code": 0},
                "artifact": ARTIFACT,
                "kept": [[0, 5], [10, 15]]}),
            Some(ACTION),
        ))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert!(
        app.call_view_open(),
        "the view stays open across completion"
    );
    let view = app.call_view().expect("the view stays open");
    assert_eq!(view.artifact.as_deref(), Some(ARTIFACT));
    assert_eq!(view.kept, Some(vec![[0, 5], [10, 15]]));
    let request = sends
        .iter()
        .find(|line| line["command"] == json!("artifact_page"))
        .expect("the completion pumps the new artifact fetch: {sends:?}")
        .clone();
    assert_eq!(request["args"]["offset"], json!(0));
    assert_eq!(request["args"]["path"], json!(ARTIFACT));
    assert!(
        app.on_line(accepted(&request, "head\ntail\n", vec![0, 5], None, 10))
            .is_empty()
    );
    let rows = frame_text(&app);
    assert!(rows.iter().any(|row| row.contains("head")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("tail")), "{rows:?}");
}

#[test]
fn a_view_opened_on_a_running_call_shows_a_whole_result_when_it_completes() {
    // No artifact: the completion's uncapped rows draw whole at once,
    // with no fetch.
    let mut app = attached(80, 24);
    turn_started(&mut app);
    requested(&mut app, "shell", json!({"command": COMMAND}), ACTION);
    let id = crate::turn::target_id(ACTION);
    app.open(Target::Group(id));
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    let sends: Vec<Value> = app
        .on_line(envelope(
            "tool_call_completed",
            json!({"status": "completed",
                "content": [{"type": "text", "text": "one\ntwo"}],
                "details": {"command": COMMAND, "exit_code": 0}}),
            Some(ACTION),
        ))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert!(sends.is_empty(), "no artifact means no fetch: {sends:?}");
    let rows = frame_text(&app);
    assert!(rows.iter().any(|row| row.contains("one")), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("two")), "{rows:?}");
}

#[test]
fn below_len_counts_the_notice() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let view = app.call_view().expect("open");
    assert_eq!(view.below_len(), 0, "no notice leaves the height whole");
    let sends = answer_page(&mut app, &first, &text, 0);
    let next = page_next(&text, 0).expect("first page names next");
    let second = sends
        .iter()
        .find(|line| line["args"]["offset"] == json!(next))
        .expect("follow-up")
        .clone();
    assert!(app.on_line(rejected(&second, "io_failed")).is_empty());
    let view = app.call_view().expect("open");
    assert_eq!(view.below_len(), 1, "one notice takes one row");
    let frame = view.frame();
    assert!(frame.below.iter().any(|line| line.contains("io_failed")));
}

#[test]
fn opening_while_disconnected_notices_and_sends_nothing() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    app.disconnected();
    assert_eq!(app.open_call_view(id), Effect::None);
    let view = app.call_view().expect("open");
    assert!(
        view.notice
            .as_ref()
            .is_some_and(|notice| notice.contains("not connected")),
        "disconnected open notices: {:?}",
        view.notice
    );
}

#[test]
fn opening_while_connected_sends_without_a_notice() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    let view = app.call_view().expect("open");
    assert!(view.notice.is_none(), "connected open has no notice");
    assert_eq!(request["args"]["offset"], json!(0));
}

#[test]
fn keys_without_a_view_and_ctrl_c_with_one_go_nowhere() {
    let mut app = attached(80, 24);
    assert_eq!(app.call_view_key(&Key::Esc), None);
    assert_eq!(app.call_view_key(&Key::Up), None);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let _ = page_request(app.open_call_view(id));
    assert_eq!(app.call_view_key(&Key::CtrlC), None);
}

#[test]
fn arrows_without_find_scroll_instead_of_stepping_matches() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let _ = page_to_end_from_zero(&mut app, first, &text);
    let before = app.call_view().expect("open").list.selected();
    assert!(before > 0, "following starts at the bottom: {before}");
    app.on_key(Key::Up, now());
    let after = app.call_view().expect("open").list.selected();
    assert_eq!(after + 1, before, "Up scrolls one row when find is closed");
    assert!(
        !app.call_view().expect("open").follow,
        "scrolling up leaves follow"
    );
}

#[test]
fn closing_focuses_the_more_row_only_past_twenty_plain_rows() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let text = (0..30)
        .map(|i| format!("row{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    requested(&mut app, "shell", json!({"command": COMMAND}), ACTION);
    completed(
        &mut app,
        ACTION,
        &text,
        json!({"command": COMMAND, "exit_code": 0}),
        None,
        None,
    );
    let id = crate::turn::target_id(ACTION);
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    let plain = app.call_view().expect("open").plain.len();
    assert!(plain > crate::turn::opened::OPENED_LINES, "{plain}");
    app.close_call_view();
    assert_eq!(
        app.focused(),
        Some(TargetId::Line(Target::CallView(id))),
        "a long call returns to its more row"
    );
}

#[test]
fn scrolling_down_mid_file_stays_unfollowed_until_the_end() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let _ = page_to_end_from_zero(&mut app, first, &text);
    assert!(app.call_view().expect("open").follow, "following starts on");
    app.on_key(Key::Up, now());
    app.on_key(Key::Up, now());
    assert!(!app.call_view().expect("open").follow, "Up leaves follow");
    let selected = app.call_view().expect("open").list.selected();
    app.on_key(Key::Down, now());
    let view = app.call_view().expect("open");
    assert_eq!(view.list.selected(), selected + 1, "Down moves one row");
    assert!(!view.follow, "Down mid-file stays unfollowed");
    for _ in 0..30 {
        app.on_key(Key::Down, now());
        if app.call_view().expect("open").follow {
            break;
        }
    }
    assert!(
        app.call_view().expect("open").follow,
        "Down to the end follows"
    );
}

#[test]
fn scrolling_down_at_the_bottom_with_no_move_stays_following() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let _ = page_to_end_from_zero(&mut app, first, &text);
    {
        let view = app.call_view_mut().expect("open");
        view.follow = false;
    }
    let selected = app.call_view().expect("open").list.selected();
    app.on_key(Key::Down, now());
    let view = app.call_view().expect("open");
    assert_eq!(
        view.list.selected(),
        selected,
        "Down at the end moves nowhere"
    );
    assert!(view.follow, "Down with no move follows");
}

#[test]
fn scrolling_up_at_the_end_does_not_refetch_the_fill() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let _ = page_to_end_from_zero(&mut app, first, &text);
    {
        let view = app.call_view_mut().expect("open");
        view.pager.fill_next = Some(99999);
        view.pager.missing.insert(99999);
    }
    let sends = sent(app.on_key(Key::Up, now()));
    assert!(
        sends
            .iter()
            .all(|line| line["args"]["offset"] != json!(99999)),
        "Up never refetches the fill: {sends:?}"
    );
}

#[test]
fn scrolling_down_at_the_end_refetches_a_missing_fill() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let _ = page_to_end_from_zero(&mut app, first, &text);
    {
        let view = app.call_view_mut().expect("open");
        view.pager.fill_next = Some(99999);
        view.pager.missing.insert(99999);
    }
    let sends = sent(app.on_key(Key::Down, now()));
    assert!(
        sends
            .iter()
            .any(|line| line["args"]["offset"] == json!(99999)),
        "Down at the end refetches the missing fill: {sends:?}"
    );
}

#[test]
fn a_wrong_answer_id_leaves_the_request_on_the_wire() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    let wrong = accepted(&request, "other\n", vec![0], None, 6);
    let mut wrong = wrong;
    if let crate::link::Line::Session(envelope) = &mut wrong {
        envelope
            .payload
            .insert("command_id".to_owned(), json!("someone-else"));
    }
    assert!(app.on_line(wrong).is_empty(), "a wrong id answers nothing");
    let view = app.call_view().expect("open");
    assert!(
        view.pager.request.is_some(),
        "the real request stays on the wire"
    );
    assert!(view.pager.pages.is_empty(), "nothing lands from a wrong id");
}

#[test]
fn an_unexpected_answer_kind_fails_without_landing() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    let wrong = envelope(
        "command_accepted",
        json!({"command_id": request["id"], "result": {"text": "x"}}),
        None,
    );
    assert!(app.on_line(wrong).is_empty());
    let view = app.call_view().expect("open");
    assert!(
        view.pager.pages.is_empty(),
        "no page lands from a wrong shape"
    );
    assert!(
        view.notice
            .as_ref()
            .is_some_and(|notice| notice.contains("unexpected answer")),
        "the view notices the wrong shape: {:?}",
        view.notice
    );
}

#[test]
fn losing_the_link_clears_the_request_so_scroll_asks_again() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let _ = page_request(app.open_call_view(id));
    assert!(app.call_view().expect("open").pager.request.is_some());
    app.disconnected();
    let view = app.call_view().expect("open");
    assert!(
        view.pager.request.is_none(),
        "the lost request leaves the wire"
    );
}

#[test]
fn pager_queue_skips_the_request_on_the_wire_but_takes_a_new_offset() {
    use crate::app::call_view_pages::Pager;
    let mut pager = Pager::new(true);
    pager.request = Some(("r".to_owned(), 7));
    pager.queue(7);
    assert!(
        pager.queued.is_empty(),
        "the offset on the wire queues nothing"
    );
    pager.queue(9);
    assert_eq!(pager.queued.iter().collect::<Vec<_>>(), [&9]);
}

#[test]
fn shown_height_leaves_top_notice_and_footer_rows() {
    use crate::app::call_view_rows::{shown_height, window};
    use crate::swapped::List;
    assert_eq!(shown_height(24, 0), 22);
    assert_eq!(shown_height(24, 1), 21);
    let rows: Vec<crate::app::call_view_rows::Row> = Vec::new();
    let list = List::default();
    assert_eq!(window(&rows, &list, 0, 24), (0, 22));
}

#[test]
fn closing_a_twenty_row_call_focuses_its_row() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    // A shell result draws its command row and its closing around the
    // output, so 18 output lines make exactly twenty plain rows.
    let text = (0..18)
        .map(|i| format!("row{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    requested(&mut app, "shell", json!({"command": COMMAND}), ACTION);
    completed(
        &mut app,
        ACTION,
        &text,
        json!({"command": COMMAND, "exit_code": 0}),
        None,
        None,
    );
    let id = crate::turn::target_id(ACTION);
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    app.close_call_view();
    assert_eq!(
        app.focused(),
        Some(TargetId::Line(Target::Call(id))),
        "exactly twenty rows is not past the cap"
    );
}

#[test]
fn stepping_wraps_both_ways_across_three_matches() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    assert!(
        app.on_line(accepted(
            &request,
            "aa\nbb\naa\nbb\naa\n",
            vec![0, 3, 6, 9, 12],
            None,
            15
        ))
        .is_empty()
    );
    type_query(&mut app, "aa");
    let matches = app.call_view().expect("open").matches.clone();
    assert_eq!(matches.len(), 3, "three rows hold aa: {matches:?}");
    assert_eq!(current_row(&app), Some(matches[0]));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(current_row(&app), Some(matches[1]), "Down steps forward");
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(current_row(&app), Some(matches[2]));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(current_row(&app), Some(matches[0]), "Down wraps to first");
    assert_eq!(app.on_key(Key::Up, now()), Effect::None);
    assert_eq!(current_row(&app), Some(matches[2]), "Up wraps to last");
}

#[test]
fn the_count_names_the_current_match_one_based() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    assert!(
        app.on_line(accepted(&request, "aa\nbb\naa\n", vec![0, 3, 6], None, 9))
            .is_empty()
    );
    type_query(&mut app, "aa");
    assert_eq!(app.call_view_count(), Some("1 of 2".to_owned()));
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    assert_eq!(app.call_view_count(), Some("2 of 2".to_owned()));
}

#[test]
fn a_new_query_resets_while_the_same_one_keeps_its_row() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    assert!(
        app.on_line(accepted(&request, "ab\nac\nabc\n", vec![0, 3, 6], None, 9))
            .is_empty()
    );
    type_query(&mut app, "a");
    // The shell prefix row holds the command's "cargo", so "a" matches
    // the prefix plus the three output rows.
    assert_eq!(app.call_view().expect("open").matches.len(), 4);
    assert_eq!(app.on_key(Key::Down, now()), Effect::None);
    let stayed = current_row(&app).expect("second match");
    assert_eq!(
        stayed,
        app.call_view().expect("open").matches[1],
        "stepped to the second row"
    );
    app.refresh_call_view_matches();
    assert_eq!(
        current_row(&app),
        Some(stayed),
        "the same query keeps its row"
    );
}

#[test]
fn a_numbered_read_gutters_single_digits_past_nine() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    requested(&mut app, "read", json!({"path": "a.txt"}), ACTION);
    completed(
        &mut app,
        ACTION,
        "l9\nl10\nl11",
        json!({"start_line": 9}),
        None,
        None,
    );
    let id = crate::turn::target_id(ACTION);
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    let rows = frame_text(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains(" 9 ") && row.contains("l9")),
        "single digits gutter to two widths past nine: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("10") && row.contains("l10")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.contains("11") && row.contains("l11")),
        "{rows:?}"
    );
}

#[test]
fn a_paged_read_numbers_the_second_page_past_sixty_four() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    requested(&mut app, "read", json!({"path": "a.txt"}), ACTION);
    completed(
        &mut app,
        ACTION,
        "cut",
        json!({"start_line": 1}),
        Some(ARTIFACT),
        None,
    );
    let id = crate::turn::target_id(ACTION);
    app.open(Target::Group(id));
    open_inline(&mut app, id);
    let first = page_request(app.open_call_view(id));
    let text = lined_file();
    let sends = answer_page(&mut app, &first, &text, 0);
    let next = page_next(&text, 0).expect("first page names next");
    let second = sends
        .iter()
        .find(|line| line["args"]["offset"] == json!(next))
        .expect("follow-up")
        .clone();
    let _ = answer_page(&mut app, &second, &text, next);
    let rows = frame_text(&app);
    assert!(
        rows.iter()
            .any(|row| row.contains("65") && row.contains("L064")),
        "{rows:?}"
    );
}

#[test]
fn only_the_current_match_draws_brightest() {
    use crate::swapped::Ink;
    use crate::theme::Role;
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    assert!(
        app.on_line(accepted(&request, "aa\nbb\naa\n", vec![0, 3, 6], None, 9))
            .is_empty()
    );
    type_query(&mut app, "aa");
    let view = app.call_view().expect("open");
    assert_eq!(view.matches.len(), 2);
    let frame = view.frame();
    let marked: Vec<Vec<Ink>> = frame
        .rows
        .iter()
        .enumerate()
        .filter(|(at, _)| view.matches.contains(at))
        .map(|(_, cells)| cells.iter().map(|(_, _, ink)| *ink).collect())
        .collect();
    assert_eq!(marked.len(), 2);
    let current = view.current.expect("current");
    for (at, inks) in view.matches.iter().zip(marked.iter()) {
        let is_current = Some(*at) == view.matches.get(current).copied();
        if is_current {
            assert!(
                inks.contains(&Ink::Marked(Role::MatchCurrent)),
                "{at} brightest"
            );
        } else {
            assert!(inks.contains(&Ink::Marked(Role::Match)), "{at} marked");
            assert!(
                !inks.contains(&Ink::Marked(Role::MatchCurrent)),
                "{at} not brightest"
            );
        }
    }
}

#[test]
fn clicking_close_shuts_while_a_body_row_only_selects() {
    use crate::swapped::Spot;
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let _ = page_request(app.open_call_view(id));
    assert!(app.call_view_open());
    app.session_view_click(Spot::Row(0));
    assert!(app.call_view_open(), "a body row keeps the view open");
    app.session_view_click(Spot::Close);
    assert!(!app.call_view_open(), "the top row closes the view");
}

#[test]
fn numbering_pins_read_but_not_shell() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    requested(&mut app, "read", json!({"path": "a.txt"}), "a_r");
    completed(&mut app, "a_r", "l9", json!({"start_line": 9}), None, None);
    let read = crate::turn::target_id("a_r");
    open_inline(&mut app, read);
    assert_eq!(app.open_call_view(read), Effect::None);
    assert_eq!(app.call_view().expect("open").numbering, Some(9));
    app.close_call_view();
    let shell = shell_whole(&mut app, ACTION);
    open_inline(&mut app, shell);
    assert_eq!(app.open_call_view(shell), Effect::None);
    assert_eq!(app.call_view().expect("open").numbering, None);
}

#[test]
fn a_hit_ending_at_a_cell_start_belongs_to_the_next_cell() {
    use crate::app::call_view_rows::Source;
    use crate::app::call_view_rows::{Row, highlight_rows};
    use crate::swapped::Ink;
    use crate::theme::Role;
    let mut rows = vec![Row {
        cells: vec![("ab".to_owned(), Ink::Plain), ("cd".to_owned(), Ink::Plain)],
        source: Source::Prefix(0),
    }];
    highlight_rows(&mut rows, "cd", Some(0), &[0]);
    let texts: Vec<String> = rows[0].cells.iter().map(|(text, _)| text.clone()).collect();
    assert_eq!(texts, vec!["ab".to_owned(), "cd".to_owned()]);
    assert!(
        rows[0]
            .cells
            .iter()
            .any(|(text, ink)| text == "cd" && *ink == Ink::Marked(Role::MatchCurrent))
    );
    assert!(
        rows[0]
            .cells
            .iter()
            .any(|(text, ink)| text == "ab" && *ink == Ink::Plain)
    );
}

#[test]
fn a_hit_spanning_two_cells_marks_both_parts() {
    use crate::app::call_view_rows::Source;
    use crate::app::call_view_rows::{Row, highlight_rows};
    use crate::swapped::Ink;
    use crate::theme::Role;
    let mut rows = vec![Row {
        cells: vec![("ab".to_owned(), Ink::Plain), ("cd".to_owned(), Ink::Plain)],
        source: Source::Prefix(0),
    }];
    highlight_rows(&mut rows, "bc", Some(0), &[0]);
    let joined: String = rows[0]
        .cells
        .iter()
        .map(|(text, _)| text.as_str())
        .collect();
    assert_eq!(joined, "abcd");
    assert!(
        rows[0]
            .cells
            .iter()
            .any(|(text, ink)| text == "b" && *ink == Ink::Marked(Role::MatchCurrent))
    );
    assert!(
        rows[0]
            .cells
            .iter()
            .any(|(text, ink)| text == "c" && *ink == Ink::Marked(Role::MatchCurrent))
    );
}

#[test]
fn markers_never_match_even_when_the_query_names_them() {
    use crate::app::call_view_rows::Source;
    use crate::app::call_view_rows::{Row, highlight_rows};
    let mut rows = vec![
        Row {
            cells: vec![(
                "── end of the model's view ──".to_owned(),
                crate::swapped::Ink::Muted,
            )],
            source: Source::Marker {
                offset: 0,
                index: 0,
                resume: false,
            },
        },
        Row {
            cells: vec![("model hello".to_owned(), crate::swapped::Ink::Plain)],
            source: Source::Prefix(0),
        },
    ];
    highlight_rows(&mut rows, "model", Some(0), &[1]);
    assert_eq!(rows[0].cells.len(), 1, "markers keep no highlight");
    assert!(
        rows[1]
            .cells
            .iter()
            .any(|(_, ink)| matches!(ink, crate::swapped::Ink::Marked(_)))
    );
}

#[test]
fn a_diff_view_keeps_its_role_colours() {
    use crate::swapped::Ink;
    use crate::theme::Role;
    let mut app = attached(80, 24);
    turn_started(&mut app);
    requested(&mut app, "edit", json!({"path": "a.txt"}), ACTION);
    completed(
        &mut app,
        ACTION,
        "",
        json!({"diff": "@@ -1 +1 @@\n-a\n+b\n"}),
        None,
        None,
    );
    let id = crate::turn::target_id(ACTION);
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    let frame = app.call_view().map(|view| view.frame()).expect("open");
    let inks: Vec<Ink> = frame
        .rows
        .iter()
        .flatten()
        .map(|(_, _, ink)| *ink)
        .collect();
    assert!(
        inks.iter()
            .any(|ink| matches!(ink, Ink::Role(Role::Removed) | Ink::Marked(Role::Removed))),
        "removed draws red: {inks:?}"
    );
    assert!(
        inks.iter()
            .any(|ink| matches!(ink, Ink::Role(Role::Added) | Ink::Marked(Role::Added))),
        "added draws green: {inks:?}"
    );
}

#[test]
fn a_search_view_keeps_its_match_tint() {
    use crate::swapped::Ink;
    use crate::theme::Role;
    let mut app = attached(80, 24);
    turn_started(&mut app);
    requested(&mut app, "shell", json!({"command": "rg foo a"}), ACTION);
    completed(
        &mut app,
        ACTION,
        "",
        json!({"command": "rg foo a", "exit_code": 0, "matches": [{"path": "a", "lines": [
            {"line": 1, "text": "aéx", "spans": [[1, 3]]},
        ]}]}),
        None,
        None,
    );
    let id = crate::turn::target_id(ACTION);
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    let frame = app.call_view().map(|view| view.frame()).expect("open");
    let inks: Vec<Ink> = frame
        .rows
        .iter()
        .flatten()
        .map(|(_, _, ink)| *ink)
        .collect();
    assert!(
        inks.contains(&Ink::Marked(Role::Match)),
        "the span tint draws marked: {inks:?}"
    );
}

#[test]
fn refresh_without_paging_keeps_fill_empty() {
    // Running to whole: neither side pages, so no fill queues.
    let mut app = attached(80, 24);
    turn_started(&mut app);
    requested(&mut app, "shell", json!({"command": COMMAND}), ACTION);
    let id = crate::turn::target_id(ACTION);
    app.open(Target::Group(id));
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    assert!(!app.call_view().expect("open").paged());
    let sends: Vec<Value> = app
        .on_line(envelope(
            "tool_call_completed",
            json!({"status": "completed",
                "content": [{"type": "text", "text": "one\ntwo"}],
                "details": {"command": COMMAND, "exit_code": 0}}),
            Some(ACTION),
        ))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert!(sends.is_empty(), "no artifact means no fetch: {sends:?}");
    let view = app.call_view().expect("the view stays open");
    assert!(!view.paged(), "still no artifact");
    assert_eq!(
        view.pager.fill_next, None,
        "an unpaged refresh queues no fill"
    );
}

#[test]
fn refresh_on_an_already_paged_view_keeps_fill_empty() {
    // Paged to paged with nothing queued: the fill stays empty.
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let _ = page_to_end_from_zero(&mut app, first, &text);
    assert!(app.call_view().expect("open").paged());
    assert_eq!(app.call_view().expect("open").pager.fill_next, None);
    let sends: Vec<Value> = app
        .on_line(envelope(
            "tool_call_completed",
            json!({"status": "completed",
                "content": [{"type": "text", "text": "cut"}],
                "details": {"command": COMMAND, "exit_code": 0},
                "artifact": ARTIFACT,
                "kept": [[0, 5]]}),
            Some(ACTION),
        ))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    let _ = sends;
    assert_eq!(
        app.call_view().expect("open").pager.fill_next,
        None,
        "an already-paged refresh queues no new fill"
    );
}

#[test]
fn refresh_with_a_queued_fill_keeps_it() {
    // Running to paged while a fill waits: the waiting offset stands.
    let mut app = attached(80, 24);
    turn_started(&mut app);
    requested(&mut app, "shell", json!({"command": COMMAND}), ACTION);
    let id = crate::turn::target_id(ACTION);
    app.open(Target::Group(id));
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    app.call_view_mut().expect("open").pager.fill_next = Some(999);
    let sends: Vec<Value> = app
        .on_line(envelope(
            "tool_call_completed",
            json!({"status": "completed",
                "content": [{"type": "text", "text": "head\ntail"}],
                "details": {"command": COMMAND, "exit_code": 0},
                "artifact": ARTIFACT,
                "kept": [[0, 5], [10, 15]]}),
            Some(ACTION),
        ))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert_eq!(
        app.call_view().expect("open").pager.fill_next,
        Some(999),
        "a queued fill survives the refresh"
    );
    assert!(
        sends
            .iter()
            .any(|line| line["args"]["offset"] == json!(999)),
        "the pump asks for the queued fill: {sends:?}"
    );
}

#[test]
fn find_call_parts_pins_action_name_and_arguments() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let arguments = json!({"command": COMMAND});
    requested(&mut app, "shell", arguments.clone(), ACTION);
    completed(
        &mut app,
        ACTION,
        "one\ntwo",
        json!({"command": COMMAND, "exit_code": 0}),
        None,
        None,
    );
    let id = crate::turn::target_id(ACTION);
    let (action, name, args) = app.find_call_parts(id).expect("the call is on the screen");
    assert_eq!(action, ACTION, "the action pins");
    assert_eq!(name, "shell", "the tool name pins");
    assert_eq!(args, arguments, "the arguments pin");
}

#[test]
fn refresh_ignores_a_completion_for_another_call() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let _ = page_request(app.open_call_view(id));
    let other = "a_2";
    requested(&mut app, "shell", json!({"command": COMMAND}), other);
    completed(
        &mut app,
        other,
        "other",
        json!({"command": COMMAND, "exit_code": 0}),
        None,
        None,
    );
    let view = app.call_view().expect("the view stays open");
    assert_eq!(
        view.artifact.as_deref(),
        Some(ARTIFACT),
        "another call's completion keeps the artifact"
    );
    assert_eq!(
        view.kept,
        Some(vec![[0, 5], [10, 15]]),
        "another call's completion keeps the kept edges"
    );
}

#[test]
fn refresh_ignores_a_completion_for_another_session() {
    use crate::link::Line;
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    open_inline(&mut app, id);
    let _ = page_request(app.open_call_view(id));
    let payload = json!({"status": "completed",
        "content": [{"type": "text", "text": "other"}],
        "details": {"command": COMMAND, "exit_code": 0}});
    let line = Line::Session(contract::Envelope {
        kind: "tool_call_completed".to_owned(),
        session_id: contract::SessionId("s_other".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: Some(contract::TurnId(TURN.to_owned())),
        action_id: Some(contract::ActionId(ACTION.to_owned())),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    });
    assert!(app.on_line(line).is_empty());
    let view = app.call_view().expect("the view stays open");
    assert_eq!(
        view.artifact.as_deref(),
        Some(ARTIFACT),
        "another session's completion keeps the artifact"
    );
}

#[test]
fn scrolling_down_to_the_bottom_follows() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let _ = page_to_end_from_zero(&mut app, first, &text);
    assert!(app.call_view().expect("open").follow);
    app.on_key(Key::Up, now());
    app.on_key(Key::Up, now());
    assert!(!app.call_view().expect("open").follow, "Up leaves follow");
    app.on_key(Key::Down, now());
    assert!(
        !app.call_view().expect("open").follow,
        "Down mid-file stays unfollowed"
    );
    app.on_key(Key::Down, now());
    assert!(
        app.call_view().expect("open").follow,
        "Down to the bottom follows"
    );
}

#[test]
fn scrolling_down_mid_file_does_not_refetch_the_fill() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let _ = page_to_end_from_zero(&mut app, first, &text);
    {
        let view = app.call_view_mut().expect("open");
        view.pager.fill_next = Some(99999);
        view.pager.missing.insert(99999);
    }
    app.on_key(Key::Up, now());
    app.on_key(Key::Up, now());
    app.on_key(Key::Up, now());
    let sends = sent(app.on_key(Key::Down, now()));
    assert!(
        sends
            .iter()
            .all(|line| line["args"]["offset"] != json!(99999)),
        "Down mid-file never refetches the fill: {sends:?}"
    );
}

#[test]
fn refresh_rebuilds_command_and_closing_from_details() {
    // Opened on a running call, the completion's details choose the
    // shell view: the command row draws before the output and the
    // closing draws after it.
    let mut app = attached(80, 24);
    turn_started(&mut app);
    requested(&mut app, "shell", json!({"command": COMMAND}), ACTION);
    let id = crate::turn::target_id(ACTION);
    app.open(Target::Group(id));
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    let sends: Vec<Value> = app
        .on_line(envelope(
            "tool_call_completed",
            json!({"status": "completed",
                "content": [{"type": "text", "text": "head\ntail"}],
                "details": {"command": COMMAND, "exit_code": 0},
                "artifact": ARTIFACT,
                "kept": [[0, 5], [10, 15]]}),
            Some(ACTION),
        ))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect();
    assert!(
        sends.iter().any(|line| line["args"]["offset"] == json!(0)),
        "the completion pumps the new artifact fetch: {sends:?}"
    );
    let view = app.call_view().expect("the view stays open");
    assert_eq!(view.title, "Shell", "the kind pins from the tool name");
    assert_eq!(view.command, COMMAND, "the target pins from the arguments");
    let rows = frame_text(&app);
    assert!(
        rows.iter().any(|row| row.contains("$ cargo test -p log")),
        "the refreshed view draws the shell command: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("exit 0")),
        "the refreshed view draws the shell closing: {rows:?}"
    );
}

#[test]
fn snapshot_completed_pins_the_call_identity() {
    let arguments = json!({"command": COMMAND});
    let done: contract::events::ToolCallCompleted = serde_json::from_value(json!({
        "status": "completed",
        "content": [{"type": "text", "text": "one\ntwo"}],
        "details": {"command": COMMAND, "exit_code": 0},
    }))
    .expect("the completion decodes");
    let snap = crate::turn::opened::snapshot_completed(42, "shell", &arguments, ACTION, &done);
    assert_eq!(snap.id, 42, "the snapshot keeps the call id");
    assert_eq!(snap.title, "Shell", "the kind pins from the tool name");
    assert_eq!(snap.command, COMMAND, "the target pins from the arguments");
    assert!(
        snap.copy.contains("one") && snap.copy.contains("two"),
        "the copy keeps the content: {:?}",
        snap.copy
    );
}

#[test]
fn a_plain_view_marks_no_match_tint() {
    use crate::swapped::Ink;
    use crate::theme::Role;
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_whole(&mut app, ACTION);
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    let frame = app.call_view().map(|view| view.frame()).expect("open");
    let inks: Vec<Ink> = frame
        .rows
        .iter()
        .flatten()
        .map(|(_, _, ink)| *ink)
        .collect();
    assert!(
        !inks.contains(&Ink::Marked(Role::Match)),
        "plain output draws no match tint: {inks:?}"
    );
}

#[test]
fn highlight_empty_hit_keeps_cells_whole() {
    use crate::app::call_view_rows::highlight_cells;
    use crate::swapped::Ink;
    let cells = vec![("ab".to_owned(), Ink::Plain)];
    let hit = 1..1;
    let out = highlight_cells(&cells, "ab", std::slice::from_ref(&hit), false);
    assert_eq!(
        out,
        vec![("ab".to_owned(), Ink::Plain)],
        "an empty hit marks nothing and splits nothing"
    );
}

#[test]
fn highlight_second_cell_gap_marks_only_its_hit() {
    use crate::app::call_view_rows::highlight_cells;
    use crate::swapped::Ink;
    use crate::theme::Role;
    let cells = vec![("ab".to_owned(), Ink::Plain), ("cd".to_owned(), Ink::Plain)];
    let hit = 3..4;
    let out = highlight_cells(&cells, "abcd", std::slice::from_ref(&hit), false);
    assert_eq!(
        out,
        vec![
            ("ab".to_owned(), Ink::Plain),
            ("c".to_owned(), Ink::Plain),
            ("d".to_owned(), Ink::Marked(Role::Match)),
        ],
        "a hit past the cell start keeps the leading plain part: {out:?}"
    );
}

/// A wheel report at (`col`, `row`): down steps toward the tail, over the
/// call view's output rows.
fn wheel(col: u16, row: u16, down: bool) -> Mouse {
    Mouse {
        kind: if down {
            MouseKind::WheelDown
        } else {
            MouseKind::WheelUp
        },
        col,
        row,
    }
}

/// The wheel over an open call view shifts its visible rows three per
/// step, never the conversation under it (`docs/tui.md`, "Swapped
/// views"; see #3578). The selection at the window's top row leaves the
/// window, so it clamps to the nearest row still on screen.
#[test]
fn wheel_over_an_open_call_view_moves_visible_rows_not_the_conversation() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let text = (0..30)
        .map(|i| format!("row{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    requested(&mut app, "shell", json!({"command": COMMAND}), ACTION);
    completed(
        &mut app,
        ACTION,
        &text,
        json!({"command": COMMAND, "exit_code": 0}),
        None,
        None,
    );
    let id = crate::turn::target_id(ACTION);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    // Leave the tail so three steps down move the selection instead of
    // following at the end.
    for _ in 0..5 {
        app.on_key(Key::Up, now());
    }
    assert!(
        !app.call_view().expect("open").follow,
        "Up leaves follow mid-file"
    );
    let before = app.call_view().expect("open").list.selected();
    let shown_top = app.call_view().expect("open").list.top();
    // Plain output pages nothing: the wheel sends nothing back.
    assert_eq!(app.on_wheel(&wheel(10, 5, true)), Effect::None);
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert_eq!(
        app.call_view().expect("open").list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        app.call_view().expect("open").list.selected(),
        shown_top + 3,
        "the selection clamps to the nearest row still on screen"
    );
    assert_eq!(before, shown_top, "the selection started on the top row");
}

/// The wheel over an open call view keeps a selection that stays on
/// screen: five Downs put it mid-window, so one step down moves the
/// window three rows under it (`docs/tui.md`, "Swapped views"; see
/// #3578).
#[test]
fn wheel_over_an_open_call_view_keeps_a_visible_selection() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let text = (0..30)
        .map(|i| format!("row{i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    requested(&mut app, "shell", json!({"command": COMMAND}), ACTION);
    completed(
        &mut app,
        ACTION,
        &text,
        json!({"command": COMMAND, "exit_code": 0}),
        None,
        None,
    );
    let id = crate::turn::target_id(ACTION);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    open_inline(&mut app, id);
    assert_eq!(app.open_call_view(id), Effect::None);
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    for _ in 0..5 {
        app.on_key(Key::Down, now());
    }
    let before = app.call_view().expect("open").list.selected();
    let shown_top = app.call_view().expect("open").list.top();
    assert_eq!(before, 5, "five Downs select the fifth row: {before}");
    assert_eq!(shown_top, 0, "the window stays at the top: {shown_top}");
    // Plain output pages nothing: the wheel sends nothing back.
    assert_eq!(app.on_wheel(&wheel(10, 5, true)), Effect::None);
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert_eq!(
        app.call_view().expect("open").list.top(),
        shown_top + 3,
        "one wheel step shifts the visible rows three"
    );
    assert_eq!(
        app.call_view().expect("open").list.selected(),
        before,
        "a selection still on screen stays where it was"
    );
}

/// The wheel over an open call view with the find bar open shifts the
/// viewport instead of stepping matches, never the conversation under
/// it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_a_call_view_with_find_open_scrolls_not_the_conversation() {
    let mut app = attached(80, 24);
    turn_started(&mut app);
    let id = shell_cut(&mut app, ACTION);
    app.on_wheel(&wheel(10, 5, false));
    assert!(app.top().is_some(), "setup scrolled the conversation up");
    open_inline(&mut app, id);
    let request = page_request(app.open_call_view(id));
    let text = "aa\nbb\n".repeat(30);
    assert!(
        app.on_line(accepted(
            &request,
            &text,
            (0..60).map(|at| at * 3).collect(),
            None,
            30,
        ))
        .is_empty()
    );
    type_query(&mut app, "aa");
    let matches = app.call_view().expect("open").matches.clone();
    assert_eq!(
        matches.len(),
        30,
        "each of thirty pairs holds aa: {matches:?}"
    );
    assert_eq!(current_row(&app), Some(matches[0]));
    let top = app.top();
    assert!(top.is_some(), "opening keeps the scrolled conversation");
    let before = app.call_view().expect("open").list.top();
    // Every page is loaded: the wheel sends nothing back. The view opens
    // at its tail, so wheel-up has room to move exactly three rows.
    assert_eq!(app.on_wheel(&wheel(10, 5, false)), Effect::None);
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert_eq!(
        app.call_view().expect("open").list.top(),
        before.saturating_sub(3),
        "the output viewport moves exactly three rows"
    );
    assert_eq!(
        current_row(&app),
        Some(matches[0]),
        "the find match selection stays unchanged"
    );
}

/// The wheel over a call view with a missing page sends the
/// `artifact_page` ask through the wheel's own return, never the
/// conversation under it (`docs/tui.md`, "Swapped views"; see #3578).
#[test]
fn wheel_over_a_call_view_with_a_missing_page_sends_artifact_page() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let sends = answer_page(&mut app, &first, &text, 0);
    // The follow-up asks for the first page's announced next offset.
    let next = page_next(&text, 0).expect("the first page names next");
    let second = sends
        .iter()
        .find(|line| line["args"]["offset"] == json!(next))
        .unwrap_or_else(|| panic!("the follow-up asks for {next}: {sends:?}"))
        .clone();
    // The second page fails, leaving the view open with a missing page.
    assert!(app.on_line(rejected(&second, "io_failed")).is_empty());
    // Scrolling the missing page into the window asks for it again.
    let mut asked = None;
    for _ in 0..30 {
        let Effect::Send(lines) = app.on_wheel(&wheel(10, 5, true)) else {
            continue;
        };
        for line in &lines {
            let line: Value = serde_json::from_str(line).unwrap_or_default();
            if line["command"] == json!("artifact_page") && line["args"]["offset"] == json!(320) {
                asked = Some(line.clone());
            }
        }
        if asked.is_some() {
            break;
        }
    }
    assert!(asked.is_some(), "the wheel asks for the missing page");
}

/// A wheel-up pauses following, a wheel-down before the tail stays
/// unfollowed, and a wheel-down reaching the tail resumes it, never the
/// conversation (`docs/tui.md:576`).
#[test]
fn wheel_up_pauses_and_down_resumes_follow_only_at_the_tail() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let _ = page_to_end_from_zero(&mut app, first, &text);
    assert!(app.call_view().expect("open").follow, "following starts on");
    let top = app.top();
    let _ = app.on_wheel(&wheel(10, 5, false));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert!(
        !app.call_view().expect("open").follow,
        "a wheel-up pauses following"
    );
    let _ = app.on_wheel(&wheel(10, 5, false));
    let _ = app.on_wheel(&wheel(10, 5, true));
    assert_eq!(app.top(), top, "the wheel left the conversation");
    assert!(
        !app.call_view().expect("open").follow,
        "a wheel-down before the tail stays unfollowed"
    );
    for _ in 0..40 {
        app.on_wheel(&wheel(10, 5, true));
        if app.call_view().expect("open").follow {
            break;
        }
    }
    assert!(
        app.call_view().expect("open").follow,
        "a wheel-down reaching the tail resumes following"
    );
    assert_eq!(app.top(), top, "the wheel left the conversation");
}

/// A wheel-down before the tail never refetches the fill, while one
/// reaching the tail does when it is missing (`docs/tui.md:576`).
#[test]
fn wheel_down_refetches_a_missing_fill_only_at_the_tail() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let _ = page_to_end_from_zero(&mut app, first, &text);
    {
        let view = app.call_view_mut().expect("open");
        view.pager.fill_next = Some(99999);
        view.pager.missing.insert(99999);
    }
    let sends = sent(app.on_wheel(&wheel(10, 5, true)));
    assert!(
        sends
            .iter()
            .any(|line| line["args"]["offset"] == json!(99999)),
        "a wheel-down at the tail refetches the missing fill: {sends:?}"
    );
    {
        let view = app.call_view_mut().expect("open");
        view.pager.fill_next = Some(99999);
        view.pager.missing.insert(99999);
    }
    app.on_wheel(&wheel(10, 5, false));
    app.on_wheel(&wheel(10, 5, false));
    assert!(
        !app.call_view().expect("open").follow,
        "wheel-ups leave the tail"
    );
    let sends = sent(app.on_wheel(&wheel(10, 5, true)));
    assert!(
        sends
            .iter()
            .all(|line| line["args"]["offset"] != json!(99999)),
        "a wheel-down before the tail never refetches the fill: {sends:?}"
    );
}
