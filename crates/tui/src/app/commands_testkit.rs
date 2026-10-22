//! The command tests’ shared fixtures: attached apps, typed keys,
//! sent lines, `commands` answers, and the rows on screen.

use crate::app::{App, Effect};
use crate::keys::Key;
use crate::link::Line;
use contract::clock::Clock;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Instant;

pub(crate) const SESSION: &str = "s_aaaaaaaaaaaaaaaa";

/// The injected clock's now.
pub(crate) fn now() -> Instant {
    fakes::clock::FakeClock::new().now()
}

/// `app` painted whole at `width` by `height`, sized first so paging
/// and framing agree: the drawn text with the shared text helper and
/// the frame's click targets.
pub(crate) fn painted(
    app: &mut App,
    width: u16,
    height: u16,
) -> (String, Vec<crate::mouse::Target>) {
    app.set_size(width, height);
    let area = ratatui::layout::Rect::new(0, 0, width, height);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    let targets = crate::view::render(app, area, &mut buf, None);
    (crate::view::text(&buf), targets)
}

/// An app in `/w`, connected to the hub, with no session.
pub(crate) fn connected() -> App {
    let mut app = App::new(PathBuf::from("/w"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    };
    assert!(app.on_line(Line::Hub(hello)).is_empty());
    app
}

/// A connected app attached to [`SESSION`].
pub(crate) fn attached() -> App {
    let mut app = connected();
    app.attach(contract::SessionId(SESSION.to_owned()));
    app
}

/// Types `text`, checking each key does nothing else.
pub(crate) fn type_text(app: &mut App, text: &str) {
    for ch in text.chars() {
        assert_eq!(app.on_key(Key::Char(ch), now()), Effect::None);
    }
}

/// Types `text` and presses Enter.
pub(crate) fn enter(app: &mut App, text: &str) -> Effect {
    type_text(app, text);
    app.on_key(Key::Enter, now())
}

/// The command lines an effect sends, parsed.
pub(crate) fn sent(effect: Effect) -> Vec<Value> {
    match effect {
        Effect::Send(lines) => lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap_or_default())
            .collect(),
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
    }
}

/// One session envelope from [`SESSION`].
pub(crate) fn session_line(kind: &str, payload: Value) -> Line {
    Line::Session(contract::Envelope {
        kind: kind.to_owned(),
        session_id: contract::SessionId(SESSION.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// A `reloaded` line from `session`.
pub(crate) fn reloaded(session: &str) -> Line {
    let mut line = session_line(
        "reloaded",
        json!({"servers": {"kept": [], "restarted": [], "started": [], "stopped": []},
            "extensions": []}),
    );
    if let Line::Session(envelope) = &mut line {
        envelope.session_id = contract::SessionId(session.to_owned());
    }
    line
}

/// The session answers the `commands` a `reloaded` asks for with skills
/// named `names`.
pub(crate) fn answer_commands(app: &mut App, names: &[&str]) {
    let asked = app.on_line(reloaded(SESSION));
    assert_eq!(asked.len(), 1);
    let asked: Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    assert_eq!(asked["command"], "commands");
    let rows: Vec<Value> = names
        .iter()
        .map(|name| json!({"name": name, "description": format!("Runs {name}."), "tag": "skill"}))
        .collect();
    let answer = session_line(
        "command_accepted",
        json!({"command_id": asked["id"], "result": {"commands": rows}}),
    );
    assert!(app.on_line(answer).is_empty());
}

/// The session answers the `commands` a `reloaded` asks for with the
/// given JSON rows, returning the asked id.
pub(crate) fn answer_rows(app: &mut App, rows: Value) -> String {
    let asked = app.on_line(reloaded(SESSION));
    assert_eq!(asked.len(), 1);
    let asked: Value = serde_json::from_str(&asked[0]).unwrap_or_default();
    assert_eq!(asked["command"], "commands");
    let id = asked["id"].as_str().unwrap_or_default().to_owned();
    let answer = session_line(
        "command_accepted",
        json!({"command_id": id, "result": {"commands": rows}}),
    );
    assert!(app.on_line(answer).is_empty());
    id
}

/// A turn starts on [`SESSION`].
pub(crate) fn turn_starts(app: &mut App) {
    let line = session_line(
        "turn_started",
        json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "hi"}]}]}),
    );
    assert!(app.on_line(line).is_empty());
}

/// The completion panel's rows.
pub(crate) fn shown(app: &App) -> Vec<String> {
    app.completions()
        .map(|completions| completions.lines())
        .unwrap_or_default()
}

/// The selected completion row's text.
pub(crate) fn selected(app: &App) -> Option<String> {
    let completions = app.completions()?;
    completions.lines().get(completions.selected?).cloned()
}

/// The built-in rows as drawn, in table order.
pub(crate) fn table_lines() -> Vec<String> {
    crate::slash::rows(&[])
        .iter()
        .map(crate::slash::Row::line)
        .collect()
}

/// Types `text`, whatever each key does.
pub(crate) fn type_any(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.on_key(Key::Char(ch), now());
    }
}

/// Types one character, returning its effect.
pub(crate) fn key(app: &mut App, ch: char) -> Effect {
    app.on_key(Key::Char(ch), now())
}

pub(crate) fn paths(list: &[&str]) -> Result<Vec<String>, String> {
    Ok(list.iter().map(|path| (*path).to_owned()).collect())
}

/// A hub line of `kind` with `payload`.
pub(crate) fn hub_line(kind: &str, payload: Value) -> Line {
    Line::Hub(contract::HubLine {
        kind: kind.to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: payload.as_object().cloned().unwrap_or_default(),
    })
}

/// Sends `hi` with no session and accepts the `start` for [`SESSION`],
/// returning the lines the acceptance sends, parsed.
pub(crate) fn start_accepted(app: &mut App) -> Vec<Value> {
    let lines = sent(enter(app, "hi"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["command"], "start");
    let accepted = hub_line(
        "command_accepted",
        json!({"command_id": lines[0]["id"], "result": {"session_id": SESSION}}),
    );
    app.on_line(accepted)
        .iter()
        .map(|line| serde_json::from_str(line).unwrap_or_default())
        .collect()
}

/// Pastes an image into the draft: Ctrl+V reads, and its result lands.
pub(crate) fn paste_image(app: &mut App) {
    assert_eq!(app.on_key(Key::CtrlV, now()), Effect::ReadImage(0));
    app.on_image(0, Ok("AAA".to_owned()));
}

/// The image part the tests paste.
pub(crate) fn image_part() -> Value {
    json!({"type": "image", "data": "AAA", "mime_type": "image/png"})
}

pub(crate) fn remote_search_app(
    workspace: &str,
) -> (
    App,
    std::sync::Arc<crate::remote::Router>,
    std::sync::mpsc::Receiver<Value>,
    std::sync::mpsc::Receiver<crate::Input>,
) {
    use std::io::{BufRead, BufReader};
    use std::sync::{Arc, mpsc};
    let router = crate::remote::Router::new();
    let (client, hub) = std::os::unix::net::UnixStream::pair().unwrap();
    router.attach(client);
    let (tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(hub).lines() {
            let Ok(line) = line else { break };
            if tx.send(serde_json::from_str(&line).unwrap()).is_err() {
                break;
            }
        }
    });
    let (tx, inputs) = mpsc::channel();
    router.set_feed(tx, crate::link::Credits::new(64));
    let mut app = connected();
    // An empty workspace is none: no hub search runs for it.
    app.workspace = (!workspace.is_empty()).then(|| PathBuf::from(workspace));
    app.set_remote(Arc::new(crate::remote::Remote::new(
        Arc::new(crate::configure_fake::Fake::new(vec![])),
        "/client".into(),
        Arc::clone(&router),
    )));
    (app, router, lines, inputs)
}

pub(crate) fn route_file_answer(
    router: &crate::remote::Router,
    line: &Value,
    kind: &str,
    result: Value,
) {
    let payload = if kind == "command_accepted" {
        json!({"command_id":line["id"],"result":result})
    } else {
        json!({"command_id":line["id"],"code":result["code"],"message":result["message"]})
    };
    let answer =
        serde_json::from_value(json!({"kind":kind,"ts":0,"schema_version":1,"payload":payload}))
            .unwrap();
    assert!(router.route(&Line::Hub(answer)));
}

pub(crate) fn fold_file_answer(
    app: &mut App,
    inputs: &std::sync::mpsc::Receiver<crate::Input>,
    deadline: &fakes::Deadline,
) {
    let crate::Input::Files { generation, result } =
        deadline.recv_or_fail(inputs, "remote file result")
    else {
        panic!("file input")
    };
    app.on_files(generation, result);
}
