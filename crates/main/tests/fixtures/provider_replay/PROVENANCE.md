# Provider replay provenance (#4098)

Each file holds the exact bytes one journey replays, loaded with
`include_bytes!` (compile-time; `docs/ci.md` forbids runtime reads of
repo files). A bug-pinned copy lives here per bug instead of pointing
at the probe file, so probe-file churn cannot move a bug's bytes.

| file | issue or probe source | source path or quote | exchange label | extraction command |
| --- | --- | --- | --- | --- |
| `3841_gateway_400.json` | #3841 | Ticket-body quote (no probe capture exists): HTTP 400 `invalid_request_error` with message `messages.164.content.0.tool_result: content cannot be empty if is_error is true` | The field exchange: the gateway answering the empty `is_error` `tool_result` request | Pinned by hand to the quoted status and message; asserted by `python3 -c` parsing the JSON and comparing `error.message` |
| `3841_text.sse` | #3841 | `research/anthropic-messages-probe/raw/stream.json` | `stream plain text`: the minimum post-fix reply, never the 400 | `python3 -c` writing the `raw_sse` of the `stream plain text` exchange |
| `3969_post_compaction_text.sse` | #3969 | Field evidence is the session log (`s_4d2ce8c10e886181`, `handoff_completed` at seq 807, `reviewer_kept` at seq 811, quoted in #3969's body) plus the request shape; reply bytes are the minimum post-compaction reply traced to `research/anthropic-messages-probe/raw/stream.json` | `stream plain text` | `python3 -c` writing the `raw_sse` of the `stream plain text` exchange |
| `bedrock_converse_text.eventstream` | `bedrock-converse-probe` (no filed bug) | `research/bedrock-converse-probe/raw/plain.eventstream`, the `plain` case (model `mistral.ministral-3-8b-instruct`, `research/bedrock-converse-probe/raw/plain.json`) | Recorded Converse text-turn bytes | `cp research/bedrock-converse-probe/raw/plain.eventstream crates/main/tests/fixtures/provider_replay/bedrock_converse_text.eventstream` |

Omitted: the `stream tool use` exchange names `get_weather` with `{}`
(`research/anthropic-messages-probe/raw/stream.json`), while Fiber
exposes an MCP tool as `mcp__<server>__<tool>` (`crates/mcp/src/name.rs`).
Bare `get_weather` can never invoke the hanging `mcp__fx__hang` fixture,
so its bytes are dropped, not served: journey A serves a scripted hang
call first and `3841_text.sse` second.
