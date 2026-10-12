//! Tests for MCP stdio line limits and child-pipe reads.

use contract::clock::Clock;

#[test]
fn max_line_is_4_mib() {
    assert_eq!(super::MAX_LINE, 4 * 1024 * 1024);
}

#[test]
fn a_line_ending_in_a_newline_strips_it() {
    // Without the `ends_with` guard the trailing newline would stay in
    // the line: the `false` mutant returns `"hi\n"` here.
    use std::io::Cursor;
    let mut reader = Cursor::new(b"hi\n".to_vec());
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line, "hi"),
        super::ReadLine::Eof => panic!("a newline-terminated line is a line, got the end"),
        super::ReadLine::TooLong => {
            panic!("a newline-terminated line is a line, got too long")
        }
    }
}

#[test]
fn a_final_line_without_a_newline_is_a_line() {
    // Without the `ends_with` guard the last byte would be popped as if
    // it were a newline: the `true` mutant returns `"h"` here.
    use std::io::Cursor;
    let mut reader = Cursor::new(b"hi".to_vec());
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line, "hi"),
        super::ReadLine::Eof => panic!("a final line without a newline is a line, got the end"),
        super::ReadLine::TooLong => {
            panic!("a final line without a newline is a line, got too long")
        }
    }
}

#[test]
fn an_empty_read_is_the_end() {
    use std::io::Cursor;
    let mut reader = Cursor::new(Vec::new());
    match super::read_line(&mut reader) {
        super::ReadLine::Eof => {}
        super::ReadLine::Line(_) => panic!("an empty read is the end, got a line"),
        super::ReadLine::TooLong => panic!("an empty read is the end, got too long"),
    }
}

#[test]
fn exactly_max_line_bytes_is_a_line() {
    // `>=` would end the reader here; only `>` lets exactly `MAX_LINE`
    // bytes through as a line.
    use std::io::Cursor;
    let content = vec![b'x'; super::MAX_LINE];
    let mut reader = Cursor::new(content);
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line.len(), super::MAX_LINE),
        super::ReadLine::Eof => panic!("exactly MAX_LINE bytes is a line, got the end"),
        super::ReadLine::TooLong => panic!("exactly MAX_LINE bytes is a line, got too long"),
    }
}

#[test]
fn max_line_content_plus_a_newline_is_a_line() {
    // The `take(MAX_LINE + 1)` lets a full line plus its newline through:
    // `-` or `*` in place of `+` truncates it and the assertion on the
    // exact length fails.
    use std::io::Cursor;
    let mut content = vec![b'x'; super::MAX_LINE];
    content.push(b'\n');
    let mut reader = Cursor::new(content);
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line.len(), super::MAX_LINE),
        super::ReadLine::Eof => panic!("MAX_LINE bytes plus a newline is a line, got the end"),
        super::ReadLine::TooLong => {
            panic!("MAX_LINE bytes plus a newline is a line, got too long")
        }
    }
}

#[test]
fn max_line_plus_one_bytes_is_too_long() {
    // `==` misses this length and `<` ends short lines instead: only `>`
    // ends exactly the lines past the cap.
    use std::io::Cursor;
    let content = vec![b'x'; super::MAX_LINE + 1];
    let mut reader = Cursor::new(content);
    match super::read_line(&mut reader) {
        super::ReadLine::TooLong => {}
        super::ReadLine::Line(_) => panic!("MAX_LINE + 1 bytes is too long, got a line"),
        super::ReadLine::Eof => panic!("MAX_LINE + 1 bytes is too long, got the end"),
    }
}

#[test]
fn a_read_error_without_bytes_is_the_end() {
    // The `true` mutant would also end a read that did carry bytes, and
    // the `false` mutant would return an empty line here.
    let mut reader = AlwaysErr;
    match super::read_line(&mut reader) {
        super::ReadLine::Eof => {}
        super::ReadLine::Line(_) => panic!("a failed read without bytes is the end, got a line"),
        super::ReadLine::TooLong => {
            panic!("a failed read without bytes is the end, got too long")
        }
    }
}

#[test]
fn a_read_error_after_bytes_keeps_the_line() {
    // The `true` mutant would discard these bytes and report the end.
    let mut reader = DataThenErr {
        data: b"hi".to_vec(),
        done: false,
    };
    match super::read_line(&mut reader) {
        super::ReadLine::Line(line) => assert_eq!(line, "hi"),
        super::ReadLine::Eof => panic!("a failed read after bytes keeps them, got the end"),
        super::ReadLine::TooLong => {
            panic!("a failed read after bytes keeps them, got too long")
        }
    }
}

/// A reader whose every read fails, carrying no bytes.
struct AlwaysErr;

impl std::io::Read for AlwaysErr {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("boom"))
    }
}

impl std::io::BufRead for AlwaysErr {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        Err(std::io::Error::other("boom"))
    }

    fn consume(&mut self, _amount: usize) {}
}

/// A reader that hands over `data` once, then fails: the line reader sees
/// a partial line followed by an error.
struct DataThenErr {
    data: Vec<u8>,
    done: bool,
}

impl std::io::Read for DataThenErr {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::BufRead;
        let chunk = self.fill_buf()?;
        let len = chunk.len().min(buf.len());
        buf[..len].copy_from_slice(&chunk[..len]);
        self.consume(len);
        Ok(len)
    }
}

impl std::io::BufRead for DataThenErr {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.done {
            Err(std::io::Error::other("boom"))
        } else {
            Ok(&self.data)
        }
    }

    fn consume(&mut self, _amount: usize) {
        self.data.clear();
        self.done = true;
    }
}

#[test]
fn a_queued_line_leaves_the_server_live() {
    let (writer, incoming) = std::sync::mpsc::sync_channel(1);
    let shared = super::super::wait::Shared::default();
    super::queue(&writer, b"hi".to_vec(), &shared);
    assert_eq!(incoming.try_recv().expect("the line arrives"), b"hi");
    assert!(
        !crate::registry::lock(&shared.inner).gone,
        "gone stays false"
    );
}

#[test]
fn a_full_queue_marks_the_server_gone() {
    let (writer, _incoming) = std::sync::mpsc::sync_channel(1);
    let shared = super::super::wait::Shared::default();
    writer
        .try_send(b"waiting".to_vec())
        .expect("one line waits");
    super::queue(&writer, b"next".to_vec(), &shared);
    assert!(crate::registry::lock(&shared.inner).gone, "gone turns true");
}

#[test]
fn a_queue_with_no_writer_marks_the_server_gone() {
    let (writer, incoming) = std::sync::mpsc::sync_channel(1);
    drop(incoming);
    let shared = super::super::wait::Shared::default();
    super::queue(&writer, b"hi".to_vec(), &shared);
    assert!(crate::registry::lock(&shared.inner).gone, "gone turns true");
}

#[test]
fn route_delivers_a_response_to_its_slot() {
    let shared = super::super::wait::Shared::default();
    let (writer, _incoming) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
    let writer = std::sync::Arc::new(writer);
    let weak = std::sync::Arc::downgrade(&writer);
    shared.insert(7);
    super::route(
        r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#,
        &shared,
        &weak,
    );
    let cancel = super::super::wait::NoCancel;
    let view = shared.view(7, &cancel);
    assert_eq!(
        view.response,
        Some(super::super::rpc::Outcome::Result(
            serde_json::json!({"ok": true})
        )),
        "the response lands in its slot"
    );
}

#[test]
fn route_answers_ping_with_an_empty_result() {
    let shared = super::super::wait::Shared::default();
    let (writer, incoming) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
    let writer = std::sync::Arc::new(writer);
    let weak = std::sync::Arc::downgrade(&writer);
    super::route(
        r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
        &shared,
        &weak,
    );
    let answer: serde_json::Value =
        serde_json::from_slice(&incoming.try_recv().expect("the ping answer is queued"))
            .expect("valid JSON");
    assert_eq!(answer["id"], serde_json::json!(3), "the id echoes");
    assert_eq!(answer["result"], serde_json::json!({}), "ping answers {{}}");
}

#[test]
fn route_answers_an_unknown_method_with_method_not_found() {
    // `elicitation/create` is answered through the elicitation path
    // now (`serve_*` below), so an unknown method names another one.
    let shared = super::super::wait::Shared::default();
    let (writer, incoming) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
    let writer = std::sync::Arc::new(writer);
    let weak = std::sync::Arc::downgrade(&writer);
    super::route(
        r#"{"jsonrpc":"2.0","id":"abc","method":"sampling/createMessage"}"#,
        &shared,
        &weak,
    );
    let answer: serde_json::Value =
        serde_json::from_slice(&incoming.try_recv().expect("the error answer is queued"))
            .expect("valid JSON");
    assert_eq!(answer["id"], serde_json::json!("abc"), "the id echoes");
    assert_eq!(answer["error"]["code"], serde_json::json!(-32601));
}

#[test]
fn route_ignores_a_non_object_line() {
    let shared = super::super::wait::Shared::default();
    let (writer, incoming) = std::sync::mpsc::sync_channel::<Vec<u8>>(8);
    let writer = std::sync::Arc::new(writer);
    let weak = std::sync::Arc::downgrade(&writer);
    super::route("not json at all", &shared, &weak);
    assert!(incoming.try_recv().is_err(), "nothing is queued");
    assert!(
        !crate::registry::lock(&shared.inner).gone,
        "an ignored line never marks the server gone"
    );
}

/// One server request over `line`, answered through `serve` alone, so the
/// reader's arms are pinned without a child.
fn served(
    method: &str,
    id: serde_json::Value,
    line: &str,
) -> (super::super::wait::Shared, Option<String>) {
    let shared = super::super::wait::Shared::default();
    let request = super::super::rpc::ServerRequest {
        id,
        method: method.to_owned(),
    };
    let answer = super::serve(&request, line, &shared);
    (shared, answer)
}

#[test]
fn serve_answers_ping_with_an_empty_result() {
    let (_shared, answer) = served(
        "ping",
        serde_json::json!(3),
        r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
    );
    let answer: serde_json::Value =
        serde_json::from_str(&answer.expect("ping is answered")).expect("valid JSON");
    assert_eq!(answer["result"], serde_json::json!({}));
}

/// One elicitation request line with `id`, asking one boolean field.
fn elicitation_line(id: &serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "elicitation/create",
        "params": {
            "message": "m",
            "requestedSchema": {
                "type": "object",
                "properties": {"a": {"type": "boolean"}},
            },
        },
    })
    .to_string()
}

#[test]
fn serve_answers_sampling_and_unknown_methods_with_method_not_found() {
    for method in ["sampling/createMessage", "roots/list", "no-such-method"] {
        let line = format!(r#"{{"jsonrpc":"2.0","id":"x","method":"{method}","params":{{}}}}"#);
        let (_shared, answer) = served(method, serde_json::json!("x"), &line);
        let answer: serde_json::Value =
            serde_json::from_str(&answer.expect("the method is answered")).expect("valid JSON");
        assert_eq!(
            answer["error"]["code"],
            serde_json::json!(-32601),
            "{method}"
        );
    }
}

#[test]
fn serve_declines_an_elicitation_with_no_asking_call() {
    for id in [serde_json::json!(7), serde_json::json!("n1")] {
        let line = elicitation_line(&id);
        let (_shared, answer) = served("elicitation/create", id.clone(), &line);
        let answer: serde_json::Value =
            serde_json::from_str(&answer.expect("no call declines at once")).expect("valid JSON");
        assert_eq!(answer["id"], id, "the id echoes");
        assert_eq!(answer["result"], serde_json::json!({"action": "decline"}));
    }
}

#[test]
fn serve_answers_a_malformed_elicitation_with_invalid_params() {
    let line = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "n2",
        "method": "elicitation/create",
        "params": {
            "message": "m",
            "requestedSchema": {"type": "object", "properties": {}},
        },
    })
    .to_string();
    let (_shared, answer) = served("elicitation/create", serde_json::json!("n2"), &line);
    let answer: serde_json::Value =
        serde_json::from_str(&answer.expect("a malformed elicitation is answered"))
            .expect("valid JSON");
    assert_eq!(answer["id"], serde_json::json!("n2"), "the id echoes");
    assert_eq!(answer["error"]["code"], serde_json::json!(-32602));
    let message = answer["error"]["message"].as_str().expect("a message");
    assert!(
        message.starts_with("Fiber cannot show this elicitation: "),
        "the message names the cause: {message}",
    );
    assert!(
        message.ends_with('.'),
        "the message ends in one period: {message}"
    );
}

#[test]
fn serve_queues_an_elicitation_with_an_asking_call() {
    let shared = super::super::wait::Shared::default();
    shared.insert(7);
    shared.register(
        7,
        contract::ActionId("a_7".to_owned()),
        fakes::clock::FakeClock::new().now() + std::time::Duration::from_secs(30),
    );
    let line = elicitation_line(&serde_json::json!("el-7"));
    let request = super::super::rpc::ServerRequest {
        id: serde_json::json!("el-7"),
        method: "elicitation/create".to_owned(),
    };
    assert!(
        super::serve(&request, &line, &shared).is_none(),
        "a queued elicitation has no answer yet",
    );
    let taken = shared.take_elicitation().expect("the queue holds it");
    assert_eq!(taken.elicitation.id, serde_json::json!("el-7"));
    assert_eq!(taken.elicitation.form.message, "m");
}
