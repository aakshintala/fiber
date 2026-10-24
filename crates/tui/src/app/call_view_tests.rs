//! Tests for a call's swapped view: opening, the frame, paging with a
//! bounded window, failures, markers, `y` and scoped search
//! (`docs/tui.md`, "Swapped views").

use std::path::PathBuf;

use serde_json::{Value, json};

use super::super::{App, Effect, Target};
use crate::keys::Key;
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

/// Pages the view to the end of `text`, answering every request in
/// order, and returns the requested offsets.
fn page_to_end(app: &mut App, first: Value, text: &str) -> Vec<u64> {
    let mut offsets = vec![first["args"]["offset"].as_u64().unwrap()];
    let mut request = first;
    for _ in 0..10 {
        let sends = answer_page(app, &request, text, offsets.last().copied().unwrap());
        if sends.is_empty() {
            break;
        }
        assert_eq!(sends.len(), 1, "{sends:?}");
        offsets.push(sends[0]["args"]["offset"].as_u64().unwrap());
        request = sends[0].clone();
    }
    offsets
}

#[test]
fn pages_land_in_order_and_evict_then_refetch_by_offset() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let size = text.len() as u64;
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let offsets = page_to_end(&mut app, first, &text);
    assert_eq!(offsets, vec![0, 320, 640], "{offsets:?}");
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
    assert!(view.segs.contains_key(&0), "extents survive paging out");
    let _ = size;
}

#[test]
fn following_many_pages_keeps_the_tail_visible_after_page_out() {
    let mut app = attached(80, 24);
    let text = (0..500).map(|i| format!("L{i:03}\n")).collect::<String>();
    let size = text.len() as u64;
    let (_id, first) = open_paged(&mut app, json!([[0, 5], [size - 5, size]]), ACTION);
    let offsets = page_to_end(&mut app, first, &text);
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
    assert_eq!(sends.len(), 1);
    let second = sends[0].clone();
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
    // Landing it fills the gap and clears the notice.
    let sends = answer_page(&mut app, &retried, &text, 320);
    assert_eq!(sends.len(), 1);
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
    assert_eq!(sends.len(), 1);
    assert!(
        app.on_line(accepted(&sends[0], "C\nDE\n", vec![100, 102], None, size))
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
    assert_eq!(sends.len(), 1);
    assert!(
        app.on_line(accepted(
            &sends[0],
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
        // One request on the wire: exactly one follow-up fetch.
        assert_eq!(sends.len(), 1, "{sends:?}");
        request = sends[0].clone();
    }
    assert_eq!(app.take_call_copy(), Some(text.clone()));
    assert!(app.copied());
    // The copy unpins its pages: none stay pinned.
    let view = app.call_view().expect("the view stays open");
    assert!(view.pinned.is_empty(), "pins release after the copy");
}

#[test]
fn y_while_a_page_is_in_flight_waits_for_the_rest_in_order() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    // The first page is on the wire when `y` is pressed: the copy waits
    // for it plus the rest, in order.
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::None);
    let offsets = page_to_end(&mut app, first, &text);
    assert_eq!(offsets, vec![0, 320, 640], "{offsets:?}");
    assert_eq!(app.take_call_copy(), Some(text.clone()));
    let view = app.call_view().expect("the view stays open");
    assert!(view.pinned.is_empty(), "pins release after the copy");
}

#[test]
fn a_failed_copy_writes_nothing_and_retries_after_scroll() {
    let mut app = attached(80, 24);
    let text = lined_file();
    let (_id, first) = open_paged(&mut app, json!([[0, 5]]), ACTION);
    let sends = answer_page(&mut app, &first, &text, 0);
    let second = sends[0].clone();
    assert_eq!(app.on_key(Key::Char('y'), now()), Effect::None);
    // The second page fails mid-copy: no partial text writes, the
    // clipboard keeps its value, and the error-code notice shows.
    assert!(app.on_line(rejected(&second, "io_failed")).is_empty());
    assert_eq!(app.take_call_copy(), None);
    assert!(!app.copied());
    assert!(
        app.call_view().expect("open").pinned.is_empty(),
        "pins release on abort"
    );
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
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert_eq!(sends[0]["args"]["offset"], json!(0));
    assert!(answer_page(&mut app, &sends[0], &text, 0).is_empty());
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
