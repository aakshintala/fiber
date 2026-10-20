# Fiber provider caching and Muse thinking

Research date: October 19, 2026.

## Summary

Fiber does not have a general missing-cache-key bug on the providers examined. Four-turn live probes hit the cache in both harnesses on OpenCode Go Muse and the OpenAI API. Their prompts, tool definitions and routing keys stayed fixed. The historical canary’s intermittent full misses are real, but this investigation did not establish their cause.

A configured Muse thinking level does reach Fiber’s request body. With `thinking=high`, Fiber sends `reasoning.effort: "high"`, matching pi. However, the two historical Muse canaries recorded no explicit Fiber level; pi recorded `high`. Unconfigured Fiber omits `reasoning`, as confirmed against a fake provider. That is a comparison-control problem, not evidence that Fiber drops a configured level.

Do not change prompt or tool ordering to chase these misses. First equalize thinking, correct miss classification, and capture safe prefix fingerprints during a real failing run.

## Scope and method

Source references below use these roots:

- `F/`: this repository at `dbca90b294209454e7e0eef021ec2af4a954e5eb`
- `P/`: the installed `@earendil-works/pi-coding-agent` package, version `1.2.0`
- `A/`: `P/node_modules/@earendil-works/pi-ai/dist/`

The detailed Fiber comparison and build used `F/`. No source changes or commits were made. No GitHub content was posted. Issue [switchyard #204](https://github.com/aakshintala/switchyard/issues/204) and its comments were read. Its current bar separates main-agent cost from reviewer cost; this report does not treat reviewer spending as a provider-cache defect.

There are no `docs/providers*.md` files at this Fiber revision. The relevant current documents are `F/docs/model-routing.md`, `F/docs/prompt-cache.md` and `F/docs/performance.md`. The performance budgets cover local process costs, not provider cache retention (`F/docs/performance.md:12-30,61-91`).

Probe procedure:

- built `cargo build --bin fiber` in the fresh worktree, with a 1,200-second command timeout
- used isolated Fiber and pi homes and an inert workspace under `/tmp`, not the owner’s session/configuration directories
- copied provider packages from the pinned Fiber worktree, changing only the local credential source, endpoint allowlist and base URL for recording
- routed both CLIs through one HTTP/1.1 loopback recording server at `127.0.0.1:18741`
- forwarded Muse to `https://opencode.ai/zen/go/v1/responses` and OpenAI to `https://api.openai.com/v1/responses`, using their probe key files without printing keys
- ran four user turns per harness/provider: 200 numbered reference-data lines plus “Reply only OK. Do not call tools.”, followed by three short requests for OK
- resumed the same session between turns; Fiber used `ask --resume`, pi used `--session-id`
- set `thinking=high` in both harnesses; disabled pi’s discovered extensions, skills, context files and MCP, but retained its built-in tools
- capped live output at 256 tokens in the recording proxy, identically on every turn; otherwise forwarded the captured JSON fields unchanged
- captured codex with a fake Responses provider and a synthetic account token, not a live ChatGPT subscription

The tables show provider-reported usage, not estimates from body length. There were 16 successful live calls, plus 4 rejected OpenAI setup calls. The latter resulted from the proxy accidentally duplicating `Content-Type`; they returned HTTP 400 before inference and are excluded. Initial Fiber setup failures never reached a provider. Initial fake codex attempts failed locally until the recorder decoded pi’s zstd body. These were probe defects, not Fiber defects.

Each CLI subprocess had a 600-second timeout. No test suite was run. The proxy reused its upstream connections and ran sessions in bounded batches. Sampled machine-wide TCP `TIME_WAIT` counts were 355 before, 31/46 during, and 25 after probing—below 1,000. The build’s `target/` was removed after gathering evidence. The excerpts and fingerprints below are the durable capture record; temporary probe files are not part of the deliverable.

## Question 1: request differences and cache misses

### Answer

Both harnesses send stable cache keys for Muse and OpenAI. Fiber sends codex’s required session header too. None of those keys changes per turn in the captured sessions.

There are wire differences, but the live results do not identify them as cache-miss causes:

| Field or behavior | Fiber | pi | Assessment |
|---|---|---|---|
| Muse routing | `x-opencode-session` and body `prompt_cache_key` | same, plus `x-client-request-id` | Required OpenCode routing header is present in both. |
| Muse client attribution | `User-Agent: fiber/0.0.0` | pi user agent and `x-opencode-client: pi` | Stable client identity; no evidence this explains misses. Do not impersonate pi. |
| OpenAI API routing | body `prompt_cache_key`; no session header | body key, `session_id`, `x-client-request-id` | Fiber hit without the additional headers in the live probe. |
| Codex SSE routing | `session_id`, body `prompt_cache_key` | `session-id`, `x-client-request-id`, body key | Both preserve a session identity. Header spellings differ; Fiber’s earlier live research tested both spellings. Current codex probe is fake-only. |
| System prompt, Muse/OpenAI API | top-level `instructions` | leading `input` item with role `developer` | Both layouts cached successfully. They differ across harnesses, not across turns. |
| System prompt, codex | top-level `instructions` | top-level `instructions` | Same placement. |
| JSON object order | sorted keys | insertion order | Stable within each harness. JSON byte order is not itself the vendor’s token-prefix order. |
| Tool array | sorted by tool name | declaration order; `read,bash,edit,write` here | Different but stable. Tool lists and schemas also differ between harnesses. |
| Tool choice | explicit `"auto"` | omitted on Muse/OpenAI API; explicit `"auto"` on codex | Fixed within every session. |
| Reasoning, explicit high | `{"effort":"high"}` | `{"effort":"high","summary":"auto"}` | Same effort, different summary request. |
| `store` | false for OpenAI/codex; omitted for Go Muse | false | Muse parity/privacy setting worth making explicit; not a demonstrated cache fix. |
| Output cap at harness boundary | omitted unless explicitly requested | model maximum, 131072 for Muse, 128000 for Luna | The proxy replaced either value with 256 for all live calls. No cap-based conclusion follows. |
| Codex extras | no explicit `parallel_tool_calls` or text verbosity | `parallel_tool_calls:true`, `text.verbosity:"low"` | Possible behavior/throughput difference, not a demonstrated cache cause. |
| Codex transport | uncompressed SSE JSON | zstd-compressed SSE JSON in this probe | Compare decompressed JSON, not compressed prefixes. |
| Retention | does not send Responses retention fields | neither live probe sent retention fields | Fiber’s `cache.lifetime=1h` is not a promise of a one-hour Muse/OpenAI cache. |

### Source evidence

Fiber’s shared Responses encoder builds `instructions`, `input`, `tools`, `tool_choice`, `prompt_cache_key`, encrypted-reasoning inclusion and optional reasoning/store in `F/crates/provider/src/openai_responses.rs:128-184`. It sorts tools in `:103-125`. The common headers, including the declared cache-key header, are built in `F/crates/provider/src/call.rs:610-644`.

The loop keeps the root session ID as `cache_key` (`F/crates/loop/src/lib.rs:162-163,458-461,555`) and reuses it in every model request (`F/crates/loop/src/handoff.rs:183-200`). The same request copies the built preamble rather than rediscovering tools each turn. Responses replays same-model assistant and reasoning provider items (`F/crates/provider/src/openai_responses.rs:222-263`), rather than regenerating old reasoning text.

Go Muse declares `cache_key_header: "x-opencode-session"` in `F/providers/opencode/providers/opencode-go.json:797-827`. Codex declares `session_id`, `store:false` and `originator:fiber` in `F/providers/codex/providers/codex.json:1-11`. OpenAI models declare `store:false`; for example `F/providers/openai/providers/openai.json:1-11`.

Pi’s Go provider wraps Responses with its session-header adapter (`A/providers/opencode-go.js:9-25`; `A/providers/opencode-headers.js:1-23`). The coding-agent layer independently adds OpenCode session/client headers (`P/dist/core/provider-attribution.js:48-58`). The generic Responses client adds session affinity and request-ID headers (`A/api/openai-responses.js:190-222`). Its body builder and reasoning mapping are at `:225-295`.

Pi’s generic Responses message conversion emits the leading developer/system item (`A/api/openai-responses-shared.js:123-139`). Codex instead excludes that item and supplies `instructions` separately (`A/api/openai-codex-responses.js:379-400`). Its effort and summary fields are at `:415-430`; its SSE headers are at `:1282-1309`; compression is at `:122-138,249-255`.

Fiber’s existing cache policy requires stable definitions and settings (`F/docs/prompt-cache.md:14-75`). Its earlier codex experiments found that `session_id` matters, while `prompt_cache_key` alone was insufficient (`F/docs/prompt-cache.md`, “Cache markers and keys”). That historical evidence is not a substitute for a new live codex capture.

### Captured headers and body excerpts

All values below stayed identical across the four turns except body content length and appended conversation items. Authentication is redacted. The loopback host is not the production host.

```text
Fiber Go Muse:
  authorization: [REDACTED]
  user-agent: fiber/0.0.0
  accept: text/event-stream
  x-opencode-session: s_cd536caaa85715e7
  body: {"model":"muse-spark-1.3-contributor",
         "prompt_cache_key":"s_cd536caaa85715e7",
         "reasoning":{"effort":"high"},"tool_choice":"auto",
         "include":["reasoning.encrypted_content"],"stream":true,
         "instructions":"[fixed system prompt]",
         "input":[{"role":"user","content":[...]}, ...],"tools":[...]}
  store: absent

pi Go Muse:
  authorization: [REDACTED]
  user-agent: pi (darwin 25.6.0; arm64)
  accept: application/json
  x-opencode-session: cache-muse-b1a3f228
  x-client-request-id: cache-muse-b1a3f228
  x-opencode-client: pi
  body: {"model":"muse-spark-1.3-contributor",
         "prompt_cache_key":"cache-muse-b1a3f228","store":false,
         "reasoning":{"effort":"high","summary":"auto"},
         "input":[{"role":"developer","content":"[fixed system prompt]"}, ...],
         "include":["reasoning.encrypted_content"],"stream":true,"tools":[...]}

Fiber OpenAI API:
  authorization: [REDACTED]
  session_id: absent
  x-client-request-id: absent
  body: {"model":"gpt-6-luna","prompt_cache_key":"s_96d9a3d84acc1247",
         "reasoning":{"effort":"high"},"store":false,...}

pi OpenAI API:
  authorization: [REDACTED]
  session_id: cache-openai-2fe57e81
  x-client-request-id: cache-openai-2fe57e81
  body: {"model":"gpt-6-luna","prompt_cache_key":"cache-openai-2fe57e81",
         "reasoning":{"effort":"high","summary":"auto"},"store":false,...}

Fiber codex, fake provider:
  authorization: [REDACTED]
  originator: fiber
  session_id: s_883dc521eedb33ef
  body: {"prompt_cache_key":"s_883dc521eedb33ef",
         "reasoning":{"effort":"high"},"store":false,"tool_choice":"auto",...}

pi codex, fake provider:
  authorization: [REDACTED synthetic token]
  chatgpt-account-id: [REDACTED synthetic account]
  originator: pi
  openai-beta: responses=experimental
  session-id: cache-fake-48f16b27
  x-client-request-id: cache-fake-48f16b27
  content-encoding: zstd
  decompressed body: {"prompt_cache_key":"cache-fake-48f16b27","store":false,
         "reasoning":{"effort":"high","summary":"auto"},
         "parallel_tool_calls":true,"text":{"verbosity":"low"},...}
```

Pi also sent stable `x-stainless-*` SDK/platform headers, `accept-language`, `sec-fetch-mode` and `accept-encoding`. Fiber did not. None supplies a changing conversation/cache identity in these captures.

### Prefix stability evidence

`B` below is the captured JSON body before the proxy’s output cap. For codex, it is the decompressed JSON body.

- byte prefix: SHA-256 of the first 1,024 UTF-8 bytes of `B`
- static projection: SHA-256 of `B` with only top-level `input` removed, using recursively sorted object keys and compact JSON
- append check: the next request’s first N `input` items exactly equal all N items of the previous request

The reconstructed compact body matched the recorded full-body SHA-256 for every request. This verifies the byte-prefix calculation rather than assuming equivalent reserialization. Static projections include the complete tool definitions, instructions where present, effort and cache key. Pi’s generic system prompt lives in `input`; its first item was independently unchanged.

| Capture | Turns | Byte-prefix SHA-256 on every turn | Static-projection SHA-256 on every turn | Append checks |
|---|---:|---|---|---|
| Fiber Muse | 1–4 | `49ec0b8eea1f7d47b1be0117726be96cb60949dad3cac796e201f155c2b4b8b3` | `6166e3ee56acc6ceea2961083e80ef794baa3f66d2d4d2ab53048cc4d3f65149` | 3/3 pass |
| pi Muse | 1–4 | `5d0929240133674624556bb97a794f53f68507da5ff72039ff839f635f58b06c` | `51212f961023bea0ee25c58cd0726ae22bd24fca71e7e630030e4ed7149dc772` | 3/3 pass |
| Fiber OpenAI | 1–4 | `40f7a0e3f4a05b1ff0209ab0bcd902e1a0e7f16b48ee9add383964c487414c6f` | `90caa056d47fde668ec86b8634eb312a575d3376a1210cb4c63f0647c54a2cc5` | 3/3 pass |
| pi OpenAI | 1–4 | `8487d1585f37bf64bc4c7765c7981f5e7462e9ec2adc6e7018eea716cb1a5821` | `2ba6f3865570f2dfed83e65632b3469c4a1a507124eb8d4c109c38895c6867c7` | 3/3 pass |
| Fiber codex fake | 1–4 | `e19f64764b4bded7d53f1fcb21904b9e020a4fede0b1259be8d425e9ded22bbd` | `515ea6155a34634e8caa6e081e534558e37d1a4b94f1615f27a7602147e55fb4` | 3/3 pass |
| pi codex fake | 1–4 | `072111b2616096cd2de5c6aca313ebafe9ffa4dd9ae8000e2ad9267310a603f6` | `a167100f92fcc2ad8353ad4086b3ce8ee2f4123fe3b41ff07a6c2d90b5230d90` | 3/3 pass |

All six sessions kept their system prompt and tool-definition hashes fixed. Full-body hashes changed as expected when input was appended. The fake codex replies prove serialization and stability only, not cache acceptance or retention.

### Live cache-hit tokens per turn

Each cell is `total input / cached input`, copied from `response.usage.input_tokens` and `input_tokens_details.cached_tokens`. Total input includes cached tokens. These isolated default-install probes do not reproduce the original canary’s prompt sizes, installed resources or workload, so their absolute sizes are not a new harness-overhead comparison.

| Provider and harness | Turn 1 | Turn 2 | Turn 3 | Turn 4 |
|---|---:|---:|---:|---:|
| Go Muse, Fiber | 9652 / 0 | 9697 / 9585 | 9731 / 9585 | 9764 / 9713 |
| Go Muse, pi | 4802 / 113 | 4861 / 4721 | 4900 / 4849 | 4934 / 4849 |
| OpenAI Luna API, Fiber | 12725 / 0 | 12782 / 12722 | 12816 / 12779 | 12840 / 12813 |
| OpenAI Luna API, pi | 4134 / 0 | 4158 / 4131 | 4193 / 4155 | 4229 / 4190 |
| Codex, either harness | unknown: fake | unknown: fake | unknown: fake | unknown: fake |

Muse’s small first-turn pi hit does not mean that the whole session was previously cached. It is only 113 tokens. Cache reuse on turns 2–4 is the relevant observation.

The OpenAI responses also reported `cache_write_tokens`: Fiber `12722,57,34,24`; pi `4131,24,35,36`. Fiber’s current Responses decoder ignores that field, subtracting cached reads only and always recording an empty write map (`F/crates/provider/src/openai_responses.rs:684-694`). Pi subtracts reads and writes separately (`A/api/openai-responses-shared.js:440-450`). This is a distinct usage-accounting discrepancy, not a cache-read miss. It does not invalidate the raw provider counts above.

### What the historical canary actually shows

The canary runs show intermittent full misses in both Fiber and pi, not a universal cache failure and no fixed expiry: some misses followed gaps under a minute, one gap of about six minutes still hit, and no model switch, reload or handoff preceded them. One apparent miss was an HTTP 504 attempt that reported no usage, so a failed attempt must not count as a miss. The per-record log evidence stays out of this repository.

## Question 2: whether Muse thinking reaches the wire

### Answer

Yes, when configured. Both harnesses sent `high` in all four live Muse requests. Fiber did not drop, rename or downshift it. Pi additionally requested a reasoning summary.

Without a configured level, Fiber’s Go Muse model has no declared default. Fiber then omits `reasoning`; the vendor chooses its own behavior. The two historical Muse canaries used that unconfigured state, while pi logged `high`. The vendor’s effective effort for an omitted field is unknown.

### Evidence

The live captured fields were:

```json
{"harness":"Fiber","turns":[1,2,3,4],"reasoning":{"effort":"high"}}
{"harness":"pi","turns":[1,2,3,4],"reasoning":{"effort":"high","summary":"auto"}}
```

A separate fake-provider request used the same pinned Fiber binary and Go Muse definition, with no `thinking` configuration or model suffix. The captured body contained:

```json
{"model":"muse-spark-1.3-contributor",
 "prompt_cache_key":"s_237cc3fb940ac6b9",
 "include":["reasoning.encrypted_content"],
 "stream":true,"tool_choice":"auto"}
```

`reasoning` and `reasoning_effort` were both absent. The fake response ended the turn successfully. This proves omission at the wire boundary; it does not reveal the real vendor’s default.

The full selection and transmission path is:

- `F/crates/main/src/settings.rs:97-143`: suffix/session choice, then valid configured level, otherwise `model.thinking_default`
- `F/crates/main/src/parts.rs:406-413,458`: select the level and put it in the prompt inputs
- `F/crates/loop/src/turn.rs:219-230`: freeze the selected level in the preamble
- `F/crates/loop/src/handoff.rs:183-194`: copy the preamble’s level into each `ModelRequest`
- `F/crates/provider/src/openai_responses.rs:156-163`: send `reasoning.effort` if the level exists

Go Muse’s generated model data has `thinking_levels` but no `thinking_default` (`F/providers/opencode/providers/opencode-go.json:797-827`). Its generator row explicitly supplies `None` as the default (`F/xtask/src/models_dev_table.rs:470-475`). Pi maps a configured high level to high in its Responses builder (`A/api/openai-responses.js:183-187,266-276`).

The canary's model strings do not specify an effort, so the Muse canaries ran Fiber with `thinking` absent while pi recorded `high`.

## Question 3: which misses Fiber can fix

### Answer

No specific historical full miss has been traced to a fixable Fiber request-field defect. Current stable-prefix captures show caching works, including when Fiber omits the extra OpenAI session headers and uses top-level `instructions`.

Fiber can fix comparison controls, measurement and observability now. It can also make Muse’s `store:false` explicit. None should be sold as a proven cure for the unexplained misses.

### Evidence and limits

The pinned request encoder already implements the critical stable-prefix rules. Its live requests hit across resumed user turns without changing the system prompt or tool definitions. The historical Muse sessions had only one `preamble_built` in the measured run. Conversely, they contain no captured wire fingerprints that can prove their old prefixes remained identical at each miss.

Fiber’s debug provider logging currently records request size, attempt, response and timing metadata, not the prompt/tool/prefix hashes needed for this diagnosis (`F/crates/provider/src/call.rs:169-186,318-335`; `F/crates/log/src/diag.rs:360-373`). Request size alone cannot distinguish a changed field from a backend cache loss.

The short-gap misses make eviction, routing changes and provider outages candidates, not findings. Likewise, successful short probes do not establish long-run retention. A meaningful next check needs a real workload capture spanning a full miss, not another change to the preamble serializer.

## Fixes

| What | Where | Expected effect |
|---|---|---|
| Pin equal thinking for the next canary, such as Muse high on both sides. Record the effective level from each session log before comparing cost. | Canary/delegate launch configuration; Fiber `-c thinking=high` or supported model suffix; canary metadata. `F/crates/main/src/settings.rs` already supports it. | Removes a demonstrated comparison mismatch. It may change reasoning, tool use and runtime; it is not itself a cache fix. Set once at session start. |
| Stop counting unavailable usage as a cache miss. Classify failed attempts separately; join usage to request outcome and keep successful retries distinct. | Canary metrics and reporting. | Removes the demonstrated HTTP 504 false miss and avoids invented token-loss totals. No provider behavior change. |
| Add opt-in, redacted cache diagnostics at the final encoded-request boundary. Include static preamble/tool hashes, cache-key identity or digest, prior-input-prefix fingerprint, request/attempt ID and raw cache usage. Never log authorization or encrypted reasoning content. | `F/crates/provider/src/call.rs` observation path and protocol encoders; `F/crates/log/src/diag.rs`. | Distinguishes prefix mutation from stable-prefix backend misses during the real canary. No system prompt or tool changes. Keep fingerprint work off the default path or benchmark its cost. |
| Make Go Muse’s stateless request intent explicit with `compat.store:false`, then verify against the real gateway. | `F/xtask/src/models_dev_table.rs` and regenerated `F/providers/opencode/providers/opencode-go.json`; shared encoder already supports it. | Matches pi’s field and Fiber’s stated stateless intent. Cache improvement is unproven. Apply to new sessions, not as a changing per-turn override. |
| Preserve OpenAI Responses `cache_write_tokens` and subtract it from uncached input when the provider reports it. Verify pricing and the proper write-lifetime representation before changing costs. | `F/crates/provider/src/openai_responses.rs:684-694`, usage contract/tests and applicable model pricing. | Fixes an observed read/write accounting difference. Does not create cache hits; does not explain the historical Muse/codex misses, whose logged writes are zero. |

Do not reorder the system prompt or tools, rotate cache keys, add per-turn timestamps, enable server-side conversation storage, or delay every call to imitate an earlier codex experiment. Those changes either lack causal evidence or would weaken the stable-session design. Extra OpenAI headers, codex `parallel_tool_calls:true`, summary requests and verbosity parity are possible controlled experiments, not established cache repairs.

## Still unknown

- The cause of the successful short-gap full misses in historical Muse 1590 and 1616. No historical wire capture proves which bytes and routing state reached the gateway.
- Whether OpenCode evicted or rerouted those sessions, or whether a provider incident contributed. A 504 is proven for one attempt, not for the other misses.
- The vendor-selected Muse effort when Fiber omits `reasoning`. The logs prove omission, not an effective low, medium or high level.
- Whether equalizing thinking changes Fiber’s turn count and tool parallelism enough to meet switchyard #204’s main-agent cost bar.
- Long-session cache retention under the current gateway. Four quick turns prove reuse, not a TTL or an eviction policy. Fiber’s configured one-hour lifetime is not sent on Responses.
- Current live codex behavior versus pi on the ChatGPT backend, including header spelling, account identity and transport effects. This report has fake wire captures and historical live counts, not a fresh subscription A/B.
- Whether the Muse `store`, summary, attribution or OpenAI header differences affect backend routing under sustained load. Both harnesses hit in the bounded live probes.
- The exact billing semantics of the newly observed OpenAI cache-write counts on the probe account. Raw counts are preserved above; no revised dollar estimate is asserted.
