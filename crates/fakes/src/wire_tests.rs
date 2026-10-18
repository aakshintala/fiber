//! The exact bytes each provider wire builder emits. The usage block in
//! the closing `response.completed` event feeds every token-count
//! assertion downstream, so any byte change here must be deliberate.

use super::{
    function_call, hello, hello_inline_completed, hello_item_done, hello_single_delta, stream,
    text_reply,
};
use serde_json::json;

/// The closing `response.completed` event on its own: `input_tokens` 10,
/// `cached_tokens` 4 and `output_tokens` 3.
const COMPLETED: &str = "event: response.completed\ndata: {\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":4},\"output_tokens\":3}},\"type\":\"response.completed\"}\n\n";

/// The `Hello.` answer in the `Hel` and `lo.` deltas, then its message
/// item and the closing event.
const HELLO: &str = concat!(
    "event: response.output_text.delta\ndata: {\"delta\":\"Hel\",\"type\":\"response.output_text.delta\"}\n\n",
    "event: response.output_text.delta\ndata: {\"delta\":\"lo.\",\"type\":\"response.output_text.delta\"}\n\n",
    "event: response.output_item.done\ndata: {\"item\":{\"content\":[{\"text\":\"Hello.\",\"type\":\"output_text\"}],\"type\":\"message\"},\"type\":\"response.output_item.done\"}\n\n",
    "event: response.completed\ndata: {\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":4},\"output_tokens\":3}},\"type\":\"response.completed\"}\n\n",
);

/// The response body as text: every builder answers a 200
/// server-sent-events stream.
#[allow(clippy::expect_used, reason = "a test helper; a failure is the test's")]
fn body(response: crate::Response) -> String {
    assert_eq!(response.status, 200);
    assert_eq!(
        response.headers,
        vec![("content-type".to_owned(), "text/event-stream".to_owned())]
    );
    String::from_utf8(response.body).expect("the wire body is UTF-8")
}

#[test]
fn empty_stream_is_only_the_completed_event() {
    assert_eq!(body(stream(&[])), COMPLETED);
}

#[test]
fn hello_answers_hello_in_two_deltas() {
    assert_eq!(body(hello()), HELLO);
}

#[test]
fn function_call_names_its_call() {
    assert_eq!(
        function_call("7", "read", &json!({"path": "w/f.txt"})),
        json!({"type": "response.output_item.done", "item": {
            "type": "function_call",
            "id": "fc_7",
            "call_id": "7",
            "name": "read",
            "arguments": "{\"path\":\"w/f.txt\"}"
        }})
    );
}

#[test]
fn text_reply_wraps_its_text() {
    assert_eq!(
        body(text_reply("Yes.")),
        concat!(
            "event: response.output_item.done\ndata: {\"item\":{\"content\":[{\"text\":\"Yes.\",\"type\":\"output_text\"}],\"type\":\"message\"},\"type\":\"response.output_item.done\"}\n\n",
            "event: response.completed\ndata: {\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":4},\"output_tokens\":3}},\"type\":\"response.completed\"}\n\n",
        )
    );
}

#[test]
fn hello_inline_completed_matches_hello_bytes() {
    assert_eq!(body(hello_inline_completed()), HELLO);
}

#[test]
fn hello_single_delta_answers_in_one_delta() {
    assert_eq!(
        body(hello_single_delta()),
        concat!(
            "event: response.output_text.delta\ndata: {\"delta\":\"Hello.\",\"type\":\"response.output_text.delta\"}\n\n",
            "event: response.output_item.done\ndata: {\"item\":{\"content\":[{\"text\":\"Hello.\",\"type\":\"output_text\"}],\"type\":\"message\"},\"type\":\"response.output_item.done\"}\n\n",
            "event: response.completed\ndata: {\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":4},\"output_tokens\":3}},\"type\":\"response.completed\"}\n\n",
        )
    );
}

#[test]
fn hello_item_done_answers_without_deltas() {
    assert_eq!(
        body(hello_item_done()),
        concat!(
            "event: response.output_item.done\ndata: {\"item\":{\"content\":[{\"text\":\"Hello.\",\"type\":\"output_text\"}],\"type\":\"message\"},\"type\":\"response.output_item.done\"}\n\n",
            "event: response.completed\ndata: {\"response\":{\"id\":\"resp_1\",\"status\":\"completed\",\"usage\":{\"input_tokens\":10,\"input_tokens_details\":{\"cached_tokens\":4},\"output_tokens\":3}},\"type\":\"response.completed\"}\n\n",
        )
    );
}
