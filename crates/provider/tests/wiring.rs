//! A request carrying `sent_tools` wires none of its tools: building its
//! call makes no large block, while building without them does. The loop
//! relies on this when it passes the built tools back in `sent_tools`
//! (`docs/events.md`, "Rewind"). This binary installs the counting
//! allocator and holds only this test, so no other test binary changes
//! allocator.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

#[path = "support/harness.rs"]
mod harness;

use std::hint::black_box;

use contract::provider::ToolDefinition;
use fakes::alloc::{Counting, large_blocks_during};
use provider::Endpoint;
use serde_json::json;

#[global_allocator]
static ALLOC: Counting = Counting;

/// A request carrying `sent_tools` wires none of its tools.
#[test]
fn a_request_with_sent_tools_wires_none_of_its_tools() {
    let ((), calibration) =
        large_blocks_during(|| drop(black_box(Vec::<u8>::with_capacity(2 << 20))));
    assert_eq!(calibration, 1, "the counting allocator is installed");
    for protocol in harness::protocols() {
        let endpoint = Endpoint {
            provider: protocol.name.into(),
            model: "m".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            direct: true,
            ..Endpoint::default()
        };
        // One tool with a 2 MiB description: wiring it copies the
        // description into the request body, a large block.
        let tool = ToolDefinition {
            name: "big".into(),
            description: "a".repeat(2 << 20),
            input_schema: json!({"type": "object"}),
            deferred: false,
            hosted: None,
        };
        let mut rewired = harness::request();
        rewired.tools = vec![tool.clone()];
        rewired.sent_tools = None;
        let mut sent = harness::request();
        sent.tools = vec![tool];
        sent.sent_tools = Some(vec![serde_json::Map::new()]);
        let (_, rewired_peak) =
            large_blocks_during(|| drop(black_box((protocol.call)(&endpoint, &rewired))));
        let (_, sent_peak) =
            large_blocks_during(|| drop(black_box((protocol.call)(&endpoint, &sent))));
        assert!(
            rewired_peak >= 1,
            "{}: wiring one 2 MiB tool makes at least one large block, got {rewired_peak}",
            protocol.name
        );
        assert_eq!(
            sent_peak, 0,
            "{}: a request with sent_tools wires none of its tools",
            protocol.name
        );
    }
}
