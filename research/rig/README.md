# Rig as a reference for Fiber's provider design

Read of the rig Rust LLM library (primary sources only) against Fiber's
settled provider design. All line numbers are from the commit pinned below;
rig moves fast, so treat exact numbers as approximate if re-checked later.

## 1. Wire protocols, mapped to Fiber's five

Rig has no fixed list of "five protocols". It has a small set of wire parsers
and a much larger set of declared "dialects" (vendor configurations) that
reuse them. Non-test line counts, mapped to Fiber's protocol names
(`docs/model-routing.md`):

| Fiber protocol | Rig files | Non-test LOC | Vendors that reuse it |
|---|---|---:|---|
| `anthropic-messages` | `providers/anthropic/{wire,completion,streaming,mod}.rs` | ~3,672 | 5 declared dialects: Anthropic itself, Zai, Minimax, Moonshot, Xiaomimimo (`providers/anthropic/wire.rs:103`, `ALL` array) |
| `openai-completions` | `providers/openai/wire.rs`, `wire/{chat,dto,route,dialects,modality}.rs`, `completion/mod.rs`, `openai/mod.rs` | ~7,096 | 21 named `Dialect` consts in `wire/dialects.rs` (OpenAI, Azure, DeepSeek, Groq, Hyperbolic, Mira, Perplexity, Together, HuggingFace, LlamaCpp, Mistral, OpenRouter, Venice, Doubleword, Zai, ZaiCoding, Minimax, MinimaxChina, Moonshot, MoonshotChina, Xiaomimimo), plus ChatGPT and Copilot's own dialect consts — 23+ total |
| `openai-responses` | `providers/openai/responses_api/{mod,streaming,wire,websocket}.rs` | ~4,967 | Same dialect table selects `Route::Responses`; OpenAI, xAI and ChatGPT default to it (`wire/dialects.rs:33`) |
| ChatGPT/codex Responses variant | `providers/chatgpt/{mod,auth/*}.rs` | ~613 | Not a separate parser — see below |
| `google-generative-ai` | `providers/gemini/{completion,streaming,mod}.rs` | ~3,002 | Gemini only; there is also a separate, newer `interactions_api` (`providers/gemini/interactions_api/`, Google's Interactions API) that is not one of Fiber's five and not needed by it |

Key finding for the codex variant: rig does **not** give ChatGPT/codex its own
parser. `providers/chatgpt/mod.rs:44` defines a `DIALECT: Dialect` constant
that sets `completion_route: Route::Responses`, a `ResponsesContract::Codex`
enum value, and OAuth. The Responses wire encoder branches on that enum in
exactly three places (`providers/openai/responses_api/wire.rs:56,165,248`),
each an `if quirks.contract == ResponsesContract::Codex` guard — always-on
streaming, no sampling/storage/metadata fields, slightly different envelope.
That is a few conditionals inside one encoder, not a second implementation.
Fiber's ADR 0007 and `docs/model-routing.md` count the ChatGPT/codex variant as
a fifth, distinct native protocol; rig's evidence is that the actual wire
delta from OpenAI Responses is small enough to be a dialect flag rather than a
new protocol. Worth a second look at whether Fiber's fifth protocol could
instead be a dialect of the fourth — but only if Fiber's own probes
(`docs/model-routing.md` cites pi as the reference) show the same small delta;
rig's delta might not match ChatGPT's actual behavior, which nobody here has
independently probed.

## 2. SSE parsing

Rig does not use `eventsource-stream` (despite what crates.io's dependency
list for `rig-core` might suggest from outside — see §7). It has its own
hand-rolled push parser: `http_client/framing.rs`.

- `SseFramer` (`framing.rs:38`) implements the WHATWG `text/event-stream`
  grammar from scratch: a byte buffer, `push(&mut self, chunk: &[u8])` feeds
  transport chunks and yields completed `SseEvent`s via `Drain`.
- Partial lines: `terminated_line()` (`framing.rs:218`) looks for `\n` or `\r`
  and only consumes a `\r` when it can see whether the next byte is `\n`,
  explicitly deferring "a split CRLF ... consumed as one terminator"
  (comment at `framing.rs:216`). Nothing is dispatched until a line is fully
  buffered.
- Multi-line `data:` fields: each `data:` line appends to a running `data`
  string with a trailing `\n` (`framing.rs:120-122`); dispatch pops the final
  `\n` before emitting, matching the spec's "join with LF, minus the last".
- Comments/keepalives: a line starting with `:` is a no-op (`framing.rs:113`).
  A leading BOM is stripped, itself tolerant of being split across chunks
  (`strip_bom`, `framing.rs:82`).
- `[DONE]`: not part of the framer — it is a protocol-level sentinel handled
  by each decoder after framing. OpenAI Chat Completions checks
  `data == "[DONE]"` (`providers/openai/wire/chat.rs:1263`) and OpenAI
  Responses checks `data.trim() == "[DONE]"` (`responses_api/streaming.rs:927`).
  Anthropic's Messages protocol has no `[DONE]`; its terminal signal is the
  `message_stop` event.
- Error event mid-stream: each protocol classifies this itself, not the
  framer. Anthropic has `"error"` in its known event-type list
  (`providers/anthropic/streaming.rs:29`) and a dedicated
  `StreamingEvent::Error { raw, .. }` arm that sets `self.failed = true` and
  emits `ProviderError::from_provider_body(raw)` (`streaming.rs:611-615`).
  OpenAI Responses classifies an `error`-typed frame via
  `classify_marker_keyed_frame::<ErrorEnvelope>(data, &["error"])`
  (`responses_api/streaming.rs:939`) and a `WHOLE_BODY_MARKERS` list that
  includes `"error"` for bodies OpenRouter-style gateways send unstreamed
  after a 200 (`responses_api/streaming.rs:851,856`).
- Connection cut mid-stream: the framer's `finish()`/`pending()` don't flush
  a partial event at EOF (doc comment, `framing.rs:2`) — an incomplete event
  is simply dropped, not synthesized. Each decoder's `finish()` then decides
  what an EOF-without-terminal-event means; see §5.

There is also `NdjsonFramer` (`framing.rs:171`) for newline-delimited JSON
(used by Ollama's own `/api/chat` wire, which is outside Fiber's five), with
the same split-CRLF care and an explicit `finish()` that returns a trailing
unterminated line — NDJSON tolerates one, SSE does not.

## 3. Streaming assembly

**Partial tool-call argument JSON and parallel tool calls.** OpenAI Chat
Completions keys an accumulator by the wire's own `index` field
(`providers/openai/wire/chat.rs:952`, `self.open_tool_calls.open(incoming.index, ...)`).
Each incoming delta is fed to `slot.observe_arguments_delta(arguments)`
(`chat.rs:986`), string-concatenating fragments until the slot's buffered
text parses as JSON. There's an explicit slot-eviction path,
`self.open_tool_calls.evict_if(incoming.index, |existing| incoming.evicts(existing))`
(`chat.rs:951`) — some vendors (comment names Mistral-style gateways
elsewhere in the file) reuse an `index` for a second call before closing the
first, so the accumulator has to detect that and force-close the evicted slot
with an empty-object fallback rather than silently merging two calls' JSON.
This is the single most concrete "vendor quirk" in the parallel-tool-call
path.

**Thinking/reasoning blocks.** Anthropic: a `ThinkingState`
(`providers/anthropic/streaming.rs:212`) accumulates `thinking_delta` text and
`signature_delta` fragments separately, because the signature can arrive
either as one value on `content_block_start` or streamed in pieces
afterward — `into_signature()` (`streaming.rs:222`) prefers the streamed
signature and falls back to the opening one. `RedactedThinking { data }`
(`completion.rs:320`) is a separate content-block variant carrying opaque
data, mapped straight through to a `reasoning_block` event
(`streaming.rs:391`).

**OpenAI encrypted reasoning / Responses `compaction` items — and whether rig
round-trips them.** Yes, explicitly, and rig treats this as load-bearing
enough to have a dedicated test file:
`providers/openai/responses_api/stateless_replay_tests.rs`, whose header
comment (lines 1-6) reads: "items a stateless Responses client must send back
unchanged: a `compaction` item round-trips verbatim on both the output and
input side of the wire; an output message's `phase` survives the trip through
rig history and is re-sent on the assistant input item, never leaked onto a
text block." Concretely: `Output::Compaction` deserializes into a
`serde_json::Map` and re-serializes to byte-identical JSON including unknown
future fields (`stateless_replay_tests.rs:13-31`, asserting `back == wire`
after a round trip through an object carrying a `future_field`). Rig's
internal `ReasoningContent` enum (`completion/message.rs:154-167`) has four
variants — `Text { text, signature }`, `Encrypted(String)`,
`Redacted { data }`, `Summary(String)` — and the OpenAI Responses adapter
converts `encrypted_content` into `ReasoningContent::Encrypted` on decode and
back into the `encrypted_content` field on encode
(`responses_api/mod.rs:612-656`), with a test asserting
"preserves non-empty encrypted content" verbatim
(`responses_api/tests.rs:2465`) and a separate test proving rig's own
minted reasoning ids for cross-provider bridging are never sent upstream
(`responses_api/tests.rs:258`, `cross_provider_minted_reasoning_ids_are_not_serialized_upstream`).

This directly answers Fiber's open question about what a resume must send
back on the Responses protocol: rig's answer is "everything opaque, byte for
byte, plus the message `phase`" — it never tries to interpret or reconstruct
an encrypted/compaction payload, only to shuttle it unchanged in both
directions.

## 4. Prompt caching

**Anthropic `cache_control`.** `Content` variants (`Text`, `Image`,
`ToolResult`, `Document`) each carry an `Option<CacheControl>` field
(`providers/anthropic/completion.rs:261-320`), settable per block via
`set_content_cache_control` (`completion.rs:1576`). Separately, rig supports
Anthropic's newer top-level automatic-caching mode: `AnthropicCompletionRequest`
has its own top-level `cache_control: Option<CacheControl>`
(`completion.rs:1568-1572`), documented as letting "the API automatically
place the cache breakpoint on the last cacheable block and advance it as the
conversation grows. No beta header required." There's a hard cap,
`MAX_CACHE_CONTROL_MARKERS: usize = 4` (`completion.rs:1584`), and a helper
`final_cacheable_tool_idx` that skips tools marked `defer_loading: true` when
picking where the last tool-side marker goes (`completion.rs:1588-1596`) —
this is the same "deferred tools sit outside the cached prefix" rule Fiber's
`docs/prompt-cache.md` states independently.

**OpenAI `prompt_cache_key`.** A plain `Option<String>` field on the Responses
request struct (`responses_api/mod.rs:1507`).

**Byte-stability.** This is where rig is explicit and opinionated in the
*opposite* direction from Fiber: the workspace root `Cargo.toml:384` pins
`serde_json = { features = ["float_roundtrip", "preserve_order"] }` — i.e.
rig deliberately turns preserve_order **on**, workspace-wide, and
`rig-cassette/Cargo.toml:54` enables it again explicitly for the same reason,
documented in `rig-cassette/src/lib.rs:17-19`: "the `http` feature enables the
native provider cassette engine, including ordered JSON maps and round-trip
float parsing." Rig's reasoning: cassette replay needs byte-identical
re-serialization of recorded JSON, and Rust's default `HashMap`/`serde_json`
map order is not stable across runs, so preserving insertion order is what
lets a captured fixture's bytes match on replay.

Fiber's `docs/prompt-cache.md` ("Bytes") states the mirror-image rule:
"serde_json's `preserve_order` feature is never enabled; a CI check fails the
build if any dependency turns it on, because Cargo features apply to the
whole build" — plus "tools go in one list sorted by name" and "every JSON
object in a tool definition is serialised with its keys sorted." Fiber's own
probe (`research/prompt-cache/probes.md`, cited from `docs/prompt-cache.md`)
found a Rust `HashMap` serializing the same eight names in a different order
across three runs, which missed the cache on two providers. Rig's own
streaming-assembly state (e.g. `reasoning_slots: HashMap<u64, BlockId>`,
`responses_api/streaming.rs:268`) uses plain `HashMap` for in-memory tool-call
tracking, which is fine — that state is never serialized to the wire — but it
means rig's byte-stability guarantee rests entirely on `preserve_order` being
on globally, a single Cargo feature flip that (as Fiber's own doc notes)
"applies to the whole build." Fiber's chosen mechanism — explicit key sorting
plus a CI gate that fails if any dependency enables `preserve_order` — is the
more defensive of the two designs: it does not depend on every future
dependency leaving a global feature off.

## 5. Errors and retries

Rig's classification is deliberately coarse and transport-shaped, not
semantic. `error.rs` defines `ErrorKind` (`error.rs:27-70`): `Http`, `Json`,
`Url`, `Request`, `Response`, `Provider`, `ProviderResponse`, `Tool(..)`,
`MemoryBackend`, `MemoryPolicy`, `Internal`, `Cancelled`, `Timeout`,
`BusClosed`, `HandlerUnavailable`, `Divergence`, `Denied`, `Other`. There is
one retry table: `retryable_status(status: Option<u16>)` (`error.rs:278-284`)
— `408`, `425`, `429` and any `5xx` are retryable, everything else (including
a missing status) is not. `transient_transport` (`error.rs:290-300`)
additionally treats a bare `StreamEnded` transport error as retryable. A
`refusal: bool` field on `ErrorReport` (`error.rs:127`) marks intentional
policy refusals as never retryable regardless of status
(`provider_response/tests.rs:443`, "a refusal is never retryable whatever its
status or transport verdict"). `Retry-After` is documented only as a
doc-comment example of how a *caller* could read it from
`non_success_headers()` (`http_client/mod.rs:88-100`) — rig itself contains no
retry loop, backoff, or automatic-wait logic anywhere in `rig-core` or
`rig-agent`; retry is entirely the embedding application's job.

Crucially: **rig has no context-overflow, quota-exceeded, rate-limited,
authentication-failed, model-not-found, or refused semantic codes anywhere in
the codebase.** A grep for `context_length_exceeded`, `invalid_prompt`,
`"prompt is too long"`, `"maximum context length"` across every non-test
`.rs` file in the workspace returns zero hits. All of that lives in whichever
status code and raw body `ProviderResponse` preserves
(`error.rs:44-48`, "a non-2xx status with a body ... Status, body, request id
and headers are on the report") — the caller is expected to pattern-match the
body itself, exactly as Fiber's own `docs/errors.md` describes doing
("Recognising a context overflow": per-provider string matches on the raw
message). Fiber has done that classification work and put it in a stable,
documented code (`context_overflow`, `quota_exceeded`, `authentication_failed`,
`model_not_found`, `refused`); rig leaves it as an exercise for every
integrator. That is a real, measured gap in rig relative to what Fiber has
already settled — not a design rig considered and rejected, just work it
has not done.

One genuine finding rig has that Fiber's `docs/errors.md` doesn't mention:
rig also retries HTTP `408` (Request Timeout) and `425` (Too Early)
(`error.rs:281`). Fiber's retry table
(`docs/errors.md`, "A failed model call") lists `rate_limited` (429),
`provider_unavailable` (5xx/529), `connection_failed`, and `stream_incomplete`
— 408 isn't named anywhere, and would presumably fall under
`connection_failed` or `invalid_request` today. Worth deciding explicitly
rather than by omission, since a provider that answers a slow request with
408 rather than dropping the connection would currently hit Fiber's "any
other 4xx" `invalid_request` bucket, which is never retried.

## 6. Vendor quirks

Rig's `Quirks` struct for the OpenAI-compatible dialect alone has 86 declared
fields (`grep -c '^    pub [a-z_]*:' providers/openai/wire.rs` → 86), each a
named bool, enum, path string or `Option`, never a name/URL string match. Each
of the 23+ dialect consts (`wire/dialects.rs`) sets only the fields that
differ from `Quirks::openai()`'s baseline via struct-update syntax — e.g.
`DEEPSEEK` (`dialects.rs:72-81`) sets `supports_response_format: false`,
`emits_complete_single_chunk_tool_calls: true`, `rewrite: BodyRewrite::DeepSeek`,
and nothing else; everything else inherits the OpenAI baseline. A separate
`BodyRewrite` enum (`wire.rs:274+`) holds the handful of quirks too
structural to express as a flag — `GroqCompoundTools` (fold
`additional_params.tools` into `compound_custom.enabled_tools` so Groq's
native tool-calling doesn't clobber function tools, and strip
`reasoning_content` on replay because Groq rejects it),
`HuggingFaceRouter` (qualify the model id for sub-providers like Fireworks),
`DeepSeek` (flatten content to a string, force `content: ""` on tool-call-only
turns, echo `index` on tool calls, suppress forced tool choice unless
thinking is off), and similarly for Mira and others.

This is a very close structural match to Fiber's own rule, stated in ADR 0007
("Every compatibility flag is declared. Fiber never infers one from a URL or
a provider id, as pi does.") and `docs/model-routing.md` ("compatibility flags
the native protocol reads ... Fiber never guesses a flag from a URL or a
provider name"). Rig independently arrived at the same discipline — a
declared-fields struct per dialect, composed by struct-update from a
baseline, with structural rewrites lifted into a small closed enum rather
than free-form code branches. The one place rig's design differs: it's all
compiled Rust consts, so adding or fixing a dialect needs a rig release;
Fiber's ADR 0007 explicitly chose the opposite (providers as installable
extensions carrying declared data, so "fixing a vendor quirk is an extension
update, not a release") specifically to avoid this. Fiber's own citation for
"pi lists 43 vendor quirks it absorbs" (ADR 0007) isn't independently
verified from this repo's research directory — no pi quirks list exists in
`research/` beyond that one-line citation — so it can't be cross-checked
against rig's 86-field count. The two numbers aren't measuring the same thing
(pi's 43 are presumably distinct behavioral fixes across all vendors and
protocols; rig's 86 are declared knob positions on one protocol's struct) but
both point the same direction: a handful of wire formats, a long tail of
per-vendor flag settings.

## 7. HTTP layer and async

The released crate and the checkout differ. `rig-core` 0.42.0 on crates.io
lists `tokio`, `reqwest`, `futures`, `async-stream` and `eventsource-stream`
as required dependencies (checked against the crates.io API on September 26,
2026). This checkout's `crates/rig-core/Cargo.toml` (`Cargo.toml:26-49`), at
commit 42f4e06 on `main` with the workspace version still 0.42.0, has no
`reqwest`, `tokio` or `eventsource-stream` in `[dependencies]`. They appear only
in `[dev-dependencies]` (`Cargo.toml:64-85`). It still requires `futures`,
`futures-timer` and `async-stream`. So the transport split described below is
on `main` and not yet released.

Concretely: rig-core defines its own transport trait,
`HttpClientExt` (`http_client/mod.rs:173`), with three methods —
`send`, `send_multipart`, `send_streaming` — each returning
`impl Future<Output = Result<...>> + WasmCompatSend`. That's an async trait,
but it names no executor; `WasmCompatSend` is rig's own marker for "Send on
native, not-necessarily-Send on wasm." The bundled reqwest transport lives in
a separate crate, `rig-reqwest` (`crates/rig-reqwest/Cargo.toml`), whose own
comment explains why tokio is there at all: "The bundled transport needs a
tokio reactor on native. When the caller has none (Bevy task pools, smol,
futures::executor), `runtime` lazily starts a single-threaded fallback
runtime and drives reqwest futures there" (`rig-reqwest/Cargo.toml:33-36`).
So tokio is a fallback *inside the reqwest adapter*, not a rig-core
requirement.

Could the protocol parsing run from blocking `std::thread` code, as Fiber
does? For the framing and decode layers, yes, straightforwardly: `SseFramer`,
`NdjsonFramer` (§2), and the per-protocol streaming decoders (Anthropic's
`StreamingEvent` handling, OpenAI's tool-call slot accumulator) are all
synchronous `push(&mut self, bytes) -> Events` state machines with no
`await` anywhere in them — they take bytes in, return parsed events out, and
would drop into a blocking read loop unchanged. The only async-native
surface in rig-core's provider code is the `HttpClientExt` trait boundary
itself (the actual network I/O), and even that is trait-generic rather than
hard-wired to tokio. This lines up with Fiber's ADR 0004 (blocking threads,
no async runtime, HTTP via ureq behind a custom connector) — nothing in
rig's protocol-parsing layer would force Fiber into an async runtime if
Fiber wanted to reuse the *approach* (not the code, which is Rust-idiomatic
but written against `impl Future`, not against a synchronous read trait).

## 8. Testing: cassettes vs Fiber's recorded/scripted streams

`rig-cassette` (crate description: "Effect-log and provider HTTP recording,
replay, scrubbing and verification for Rig") is a large, dedicated crate
(~12,485 non-test LOC across `effect_log/`, `http/`, `ecs/`, `agent/`). Its
own `Cargo.toml` comment states the corpus size directly: "2,841 provider
cassettes (50 MiB) and 842 effect goldens (17 MiB)" checked into `fixtures/`,
excluded from the published crate to stay under crates.io's 10 MiB limit
(`rig-cassette/Cargo.toml:9-19`). The engine records real HTTP exchanges
(`http/mod.rs`, 3,280 lines), replays them byte-for-byte against provider
decoders, and separately supports an "explicit destination" and "recording
guard" mode (`http/explicit_destination_tests.rs`,
`http/recording_guard_tests.rs`) to keep a test run from silently talking to
the network. `preserve_order` is enabled specifically so a replayed request
serializes to the same bytes it was recorded with (§4).

Fiber's `docs/testing.md` ("Model calls") describes essentially the same two-
source model in miniature: **recorded streams** ("real responses from each
protocol, captured with live keys ... replayed byte for byte against the
provider crate's decoders... re-recorded when a vendor change is suspected")
and **scripted streams** ("hand-written in the real wire format, for
scenarios a recording cannot produce on demand, such as a tool call, then a
429, then text"), served by a local fake server that also records what the
binary sent so tests can assert on prompt-cache byte-stability across turns.
Rig's cassette engine is the same idea at a much larger, already-mature
scale — thousands of committed fixtures across many more vendors — and
its explicit "recording guard" pattern (refuse to hit the network unless a
test opts in) is a concrete mechanism Fiber's testing doc doesn't name yet
and could borrow directly.

## 9. Learnings for Fiber

Ranked by how much each would change Fiber's provider module if adopted or
watched for.

1. **Rig's answer to "what must a resume send back" is: everything opaque,
   verbatim, both directions — never reconstructed.** `ReasoningContent`'s
   `Encrypted`/`Redacted` variants and the `stateless_replay_tests.rs` suite
   (§3) are exactly the shape of evidence Fiber's open question needs: OpenAI
   Responses' `encrypted_content` and `compaction` items, and even an output
   message's `phase`, are treated as unparseable payloads that round-trip
   byte-identically, with a dedicated test file whose only job is proving
   that. If Fiber's Responses protocol resume logic doesn't yet have an
   equivalent "send back what we don't understand, unchanged" test, this is
   the concrete template — and the risk it guards against (silently dropping
   or reformatting a field the model needs to resume its own reasoning chain)
   is real enough that rig gave it a whole test file named after the exact
   failure mode.

2. **Rig has done zero of Fiber's context-overflow/quota/auth semantic
   classification work** (§5) — this is the most measurable gap, not a
   design choice: a repo-wide grep for the strings Fiber's own probe
   (`research/provider-errors/README.md`) found (`context_length_exceeded`,
   `invalid_prompt`, "prompt is too long") returns nothing in rig outside
   tests. Fiber's `docs/errors.md` registry is already ahead of rig here;
   nothing to borrow, but confirms the work was worth doing rather than
   something a mature library would have made unnecessary.

3. **Rig's parallel-tool-call slot eviction** (`open_tool_calls.evict_if`,
   §3) is a vendor-quirk pattern worth watching for: some gateway reuses a
   tool-call `index` for a second call before the first closes. If Fiber's
   OpenAI-completions protocol accumulates tool-call argument deltas keyed
   only by index without a same-index-reuse guard, a gateway with this
   behavior would silently concatenate two calls' JSON into one. Whether any
   of Fiber's five providers/extensions actually does this hasn't been
   checked from this reading — it's a concrete thing to add to Fiber's own
   provider-quirks probe list, not a confirmed present-day bug.

4. **Fiber's `preserve_order`-never CI gate is the more defensive design**
   than rig's `preserve_order`-always choice (§4): rig's byte-stability for
   cassette replay depends on a single global Cargo feature staying on
   forever, which — as Fiber's own doc points out — "applies to the whole
   build," so any future dependency could flip it and rig would not
   necessarily notice. Fiber's explicit key-sorting plus a CI check that
   fails if `preserve_order` turns on anywhere is a stronger guarantee for
   the same problem. No change needed; this is a validation that Fiber's
   choice was the right one, with rig as the counter-example of the risk.

5. **Rig's cassette corpus scale and "recording guard" pattern** (§8) is
   worth stealing as a mechanism, not just noting: an explicit test-time
   guard that refuses a live network call unless the test opts in is cheap
   insurance against a testing.md rule ("nothing in CI calls a live
   provider") silently regressing.

6. **Rig folding the ChatGPT/codex variant into three `if` branches on the
   Responses encoder** (§1) is worth a second look, but is not strong enough
   evidence to reopen Fiber's ADR 0007 protocol count on its own — it shows
   the wire delta *rig chose to implement* is small, not that ChatGPT's
   actual backend behavior is small; nobody here has independently probed it
   the way `research/provider-errors/` probed muse and OpenRouter.

7. **Rig's protocol-parsing code (framing, decode state machines) is
   synchronous under the hood** even though its trait boundary is
   `impl Future` (§7) — this validates Fiber's ADR 0004 stance that a
   blocking-thread design loses nothing in the provider layer specifically,
   since the actual parsing work in a mature comparable library is already
   plain synchronous byte-in/event-out code.

## Sources

- rig checkout: `/Users/aakshintala/work/rig`, commit `42f4e06`, 2026-09-26
  (read-only, not modified).
  - `crates/rig-core/src/providers/{anthropic,openai,chatgpt,gemini,ollama,copilot,cohere,xai}/`
  - `crates/rig-core/src/{error.rs,http_client/mod.rs,http_client/framing.rs,wire.rs,completion/message.rs}`
  - `crates/rig-core/Cargo.toml`, `crates/rig-reqwest/{Cargo.toml,src/*.rs}`,
    `crates/rig-cassette/{Cargo.toml,src/lib.rs}`
- Fiber worktree: `/Users/aakshintala/work/fiber/.claude/worktrees/fiber-rig`
  - `docs/model-routing.md`, `docs/errors.md`, `docs/prompt-cache.md`,
    `docs/testing.md`, `docs/dependencies.md`
  - `docs/adr/0004-blocking-threads-no-async-runtime.md`,
    `docs/adr/0007-protocols-are-native-providers-are-extensions.md`
  - `research/provider-errors/README.md`
