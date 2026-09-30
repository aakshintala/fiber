# Reply repair in reference agents

What five reference agents do to a model's reply before the rest of the harness sees it: malformed tool-call argument JSON, tool calls leaked into prose, other harnesses' tool dialects, runaway repetition, and empty or refused replies. Every fact below cites a file and line or quotes a verbatim string. Supports GitHub issue #190.

## Sources

| Agent | Version | Where |
|---|---|---|
| pi | pi-coding-agent 0.87.1, pi-ai 0.87.1 | `/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/`, nested `node_modules/@earendil-works/{pi-ai,pi-agent-core}/dist` |
| Codex | `openai/codex` main at `ceea671` (2026-09-29) | `codex-rs/` |
| Claude Code | 2.1.285 | `strings` on `~/.local/share/claude/versions/2.1.285`. The code is minified, so names like `Jvt` are not stable. |
| opencode v2 | `anomalyco/opencode` branch `v2` at `74dbc50` (2026-09-29) | `packages/` |
| rig | `~/work/rig` at `42f4e06` (2026-09-26) | `crates/rig-core`, `crates/rig-agent` |
| Harness Playbook | stencil.so/blog/harness-playbook (Can Bölük, 2026-09-02) | fetched 2026-09-29 |

Paths below are relative to those roots.

## pi

Repairs:

- It repairs JSON string literals. `repairJson` "escap[es] raw control characters inside strings" and "doubl[es] backslashes before invalid escape characters" (`pi-ai/dist/utils/json-parse.js:23-70`). `parseJsonWithRepair` tries `JSON.parse` first and runs the repair only if that fails (`:71-82`).
- It salvages truncated JSON. `parseStreamingJson` falls back through strict parse, then repair, then `partial-json`, then `partial-json` on the repaired text, then `{}`. It "Always returns a valid object, even if the JSON is incomplete" (`:83-112`). Every provider uses it for final arguments as well as during streaming, for example `api/anthropic-messages.js:550` and `api/openai-completions.js:261`. So a malformed argument string never reaches the model as a parse error. It becomes whatever partial object parses, or `{}`.
- It coerces types against the schema. `validateToolArguments` removes optional `null`s (`normalizeOptionalNulls`), then calls TypeBox `Value.Convert`. For plain JSON Schema tools it also runs `coerceWithJsonSchema`: `"20"`→20, `"true"`→true, `null`→0/false/"", and number→string (`utils/validation.js:42-114, 280-298`).
- Per-tool dialect repair uses an optional `prepareArguments` hook (`pi-agent-core/dist/agent-loop.js:466-478`). Only `edit` defines one: "Some models (Opus 4.6, GLM-5.1) send edits as a JSON string instead of an array. Others send a single edit object instead of a one-element edits array." It also folds the legacy top-level `oldText`/`newText` into `edits` (`pi-coding-agent/dist/core/tools/edit.js:43-73`).
- Case-insensitive tool names apply only on the Anthropic OAuth path. `fromClaudeCodeName` maps a returned name back to the pi tool case-insensitively (`api/anthropic-messages.js:65-76, 467-469`). This undoes pi's own renaming to Claude Code casing. It is not a general alias table.

Passes back to the model as errors:

- Unknown tool: `Tool ${toolCall.name} not found`. The message does not list valid names (`agent-loop.js:481-486`).
- Schema failure: `Validation failed for tool "<name>":\n  - <path>: <message>…\n\nReceived arguments:\n<JSON>` (`validation.js:302-307`).
- Truncation: when `stopReason === "length"`, every tool call fails unexecuted with "the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments." The code gives the reason: "a truncated message can yield tool calls whose arguments parse and validate but are silently incomplete" (`agent-loop.js:160-165, 334-358`).

Refusals, empty replies and repetition:

- Anthropic `refusal` maps to `stopReason: "error"` with `stop_details.explanation` or "The model refused to complete the request". `sensitive` maps to an error. `pause_turn` maps to `stop` (`anthropic-messages.js:1181-1203`). OpenAI `content_filter` maps to an error (`openai-completions.js:1202-1203`). An error stop ends the agent loop (`agent-loop.js:143-154`).
- No detection for repetition, empty replies or tool calls leaked into text was found. A grep for repetition, loop and `<tool_call>` patterns across `dist/core`, `pi-ai` and `pi-agent-core` returned nothing relevant.

Logging:

- Only the repaired form is kept. After parsing, the raw buffer is deleted: "Finalize in-place and strip the scratch buffer so replay only carries parsed arguments" (`anthropic-messages.js:551-553`, `openai-completions.js:264-266`).

Configuration:

- JSON repair and coercion apply to every provider and model. Dialect repair is per tool, through `prepareArguments`. Nothing is configured per model.

## Codex

Repairs:

- It does no JSON repair. `parse_arguments` is `serde_json::from_str(arguments)` and maps any error to `RespondToModel("failed to parse function arguments: {err}")` (`core/src/tools/handlers/mod.rs:86-93`). `FunctionCallError::RespondToModel` goes back to the model, and `Fatal` does not (`tools/src/function_call_error.rs:5-10`).
- It tolerates the `apply_patch` dialect, and does so for every model. The comment says: "the only OpenAI model that knowingly requires lenient parsing is gpt-4.1 … we resign ourselves allowing lenient parsing for all models" (`apply-patch/src/parser.rs:47-54`). Lenient mode strips a `<<EOF`, `<<'EOF'` or `<<"EOF"` heredoc wrapper when a model passes it as a literal argv element (`:157-190, 232-253`). It also allows "leading/trailing whitespace around patch markers" (`:24-25`).
- It accepts both `apply_patch` and `applypatch` (`apply-patch/src/invocation.rs:28`).
- Shell commands that are really patches are intercepted. `intercept_apply_patch` runs on `exec_command` input (`core/src/tools/handlers/unified_exec/exec_command.rs:377`). A verified patch is applied directly. A patch that fails verification returns `"apply_patch verification failed: {parse_error}"`. A shell parse error falls through to normal execution (`core/src/tools/handlers/apply_patch.rs:489-497`).
- `exec_command` takes only `cmd: String`. No string-or-array tolerance was found (`core/src/tools/handlers/unified_exec.rs:29-30`).

Passes back to the model as errors:

- Unknown tool: `unsupported call: {tool_name}`, or `unsupported custom tool call: {tool_name}`. The message does not list valid names (`core/src/tools/registry.rs:552-570, 853-858`).
- Content filter: a `response.incomplete` with reason `content_filter` becomes `ApiError::ContentFilter`. Any other reason except `interrupted` becomes a stream error, "Incomplete response returned, reason: {reason}" (`codex-api/src/sse/responses.rs:418-434`). On a content-filter block, Codex records a developer-role `<content_filter_guidance>` message before retrying (`core/src/responses_retry.rs:66-80`). The default text is "Your previous response was blocked by a content filter. Do not treat this as a transient failure or try to reproduce or work around the blocked content…" (`prompts/src/model_messages.rs:57`). The model catalog can override it per model (`:230-237`).

Repetition and empty replies:

- No repetition or loop detection was found. A grep for `repetit|degenerate|loop detect|doom` in non-test Rust code returned only unrelated regex and test-helper hits.

Logging:

- The raw payload is logged. On an unknown tool, `tool_log_payload(&invocation.payload, …)` goes to `otel.tool_result_with_tags(…, success=false, &message …)` (`registry.rs:556-567`). Arguments stay a raw string until each handler parses them.

Configuration:

- Content-filter guidance is per model, through `catalog_messages`. `apply_patch` leniency is global.

## Claude Code

Repairs:

- It does no JSON repair. When the streamed `input` string does not parse, Claude Code fires `tengu_tool_input_json_parse_fail` and replaces the input with `{__unparsedToolInput: {raw: <first 2048 chars>, len}}` (`g2t="__unparsedToolInput"`).
- Per-tool `coerceInput` hooks run before the Zod schema parse. Each returns `{input, shapeClass, resultNote?}`. The validation function applies `e.coerceInput?.(n)` and then `e.inputSchema.safeParse(...)`. Observed hooks:
  - Edit: `replace_name`→`replace_all`, `path`→`file_path`, `old_str`→`old_string`, `new_str`→`new_string`. These are Anthropic text-editor tool names. Shape classes: `alias_replace_name`, `path`, `old_str`, `new_str`.
  - Read: a one-element array `offset` or `limit` is unwrapped. Negative `offset` is dropped. `limit<=0` is dropped. `length` is renamed to `limit` (`offset_array`, `limit_array`, `offset_neg`, `limit_dropped`, `length`).
  - Bash: `timeout_ms`→`timeout` when numeric (`timeout_ms`).
  - Write: a repeated-parameter fix, gated by a feature flag (`HBn()`), that attaches a `resultNote`: "Note: <tool>'s parameters are named `file_path` and `content`. … `<w>` repeated `<b>` and was ignored."
  - WebSearch: drops a `mode` field when that mode is off (`shapeClass:"mode_while_off"`).
- A separate `normalizeToolInput` step after parsing coerces a string `Read.offset` to a number and rewrites Bash `command` text (`"normalizeToolInput Read.offset coercion failed"`).
- Tool names match exactly or through a declared `aliases` list: `e.name===n||(e.aliases?.includes(n)??!1)`. The aliases found are Claude Code's own renames, such as `aliases:["KillShell","KillBash"]` on the task-stop tool and `aliases:["bashes"]`. There is no case-insensitive or cross-harness name matching.

Passes back to the model as errors:

- Unparseable JSON: `<tool_use_error>InputValidationError: <name> was called with input that could not be parsed as JSON.\nYou sent (first N of M bytes): <raw>\nCommon causes: unescaped backslashes in file paths (use / or \\\\), unescaped control characters, or truncated output. Retry with valid JSON.</tool_use_error>`. The transcript's `toolUseResult` is `InputValidationError: JSON parse failed (${bytes} bytes)`, and telemetry records `errorCode:"JSON_PARSE"`.
- Schema failure: `<tool_use_error>InputValidationError: …</tool_use_error>`, optionally prefixed by a per-tool `validationErrorSteer`. For a deferred tool whose schema was not sent, a hint to load it first: "typed parameters (arrays, numbers, booleans) get emitted as strings and the client-side parser rejects them. Load the tool first…".
- Unknown tool: `<tool_use_error>Error: No such tool available: <name></tool_use_error>`, with no list of valid names.
- `max_tokens`: "Claude's response exceeded the ${n} output token maximum. To configure this behavior, set the CLAUDE_CODE_MAX_OUTPUT_TOKENS environment variable."
- `refusal`: when a fallback model is armed, a `fallback_request` with `trigger:"refusal"` retries on another model. The `stop_details.category` and `explanation` are carried through.

Empty replies and repetition:

- The constant `"(no content)"` exists as the placeholder for an empty assistant message.
- No repetition detection was found in strings.

Logging:

- Every coercion emits `tengu_tool_input_coerced` with `shapeClass` and outcome `coerced_valid` or `coerced_still_invalid`. Unparseable raw input is kept truncated to 2048 characters inside the tool input.

Configuration:

- Repairs are per tool. The Write fix is flag-gated. No per-model switch was found.

## opencode v2

Repairs:

- JSON is repaired only on the native protocol path. `parseToolInput` treats `""` as `"{}"` (`packages/ai/src/protocols/shared.ts:178-185`). If strict parsing fails for a local tool, `tool-stream.ts` falls back to `partial-json` or `{}` and emits a normal `toolCall`. Only provider-executed calls fail (`packages/ai/src/protocols/utils/tool-stream.ts:4-7, 72-95`).
- The Vercel AI SDK path does not repair JSON. It emits `tool-input-error` with the `raw` input (`packages/core/src/aisdk.ts:804-824`). The session then fails the call: "Tool call arguments were malformed JSON and were not executed. Retry with valid JSON." (`packages/core/src/session/runner/publish-llm-event.ts:315-338`). There is no `experimental_repairToolCall` anywhere in `packages/`.
- Schema-guided input repair is a built-in plugin, `opencode.tool.input.repair`, on the `tool execute.before` hook. It is first in the `pre` list (`packages/core/src/plugin/internal.ts:213-214`). It repairs "only when the input schema unambiguously supports them":
  - a stringified object or array is parsed;
  - undeclared keys are dropped from a closed object;
  - an optional `null` or `{}` placeholder is removed;
  - numeric and boolean strings become numbers and booleans;
  - a scalar is wrapped into a one-item array (`{ count: "2" } -> [2]`);
  - tuples, dictionaries and `$ref`s are followed to depth 6.

  (`packages/core/src/plugin/tool-input-repair.ts:7-170`)
- There is no tool-name aliasing or lowercasing. Lookup is `direct.get(name)`. A comment notes "alias resolution, now after the repair hook", because the hook may rewrite `event.tool` (`packages/core/src/tool.ts:271-283`).

Passes back to the model as errors:

- Unknown tool: `No tool named "<name>" is currently available. Please use a tool from the available tool list.` (`packages/core/src/tool.ts:280-283`).
- Schema failure: `Invalid arguments for tool "<name>":\n- <path>: <msg>` for up to 5 issues, then "Arguments provided:\n<JSON>\n\nUpdate the arguments and call the tool again." (`packages/core/src/tool/runtime.ts:87-98`).

Repetition:

- `doom_loop` survives only as a permission key in the v1 config schema (`packages/core/src/v1/config/permission.ts:31`). No v2 code reads it.

Logging:

- The AI SDK path keeps `raw` on `tool-input-error` (`packages/ai/src/schema/events.ts:220-227`). The native path's salvaged input replaces the raw input in the `toolCall` event.

Configuration:

- The repair is a plugin that can be removed or supplemented. The JSON salvage depends on the route: native protocols salvage, the AI SDK path does not.

## rig

Repairs:

- It does no JSON repair. `parse_tool_arguments` maps empty or whitespace input to `{}` and otherwise calls `serde_json::from_str` (`crates/rig-core/src/json_utils.rs:141-149`).
- Each provider adapter chooses what happens to unparseable arguments at the end of a call through `UnparseableToolInput` (`crates/rig-core/src/streaming/mod.rs:208-226`):
  - `Drop` when "the input never fully arrived". This covers the OpenAI-compatible end-of-stream flush (`providers/openai/wire/chat.rs:1338-1341`), Cohere, and the Responses API.
  - `EmptyObject` when "the wire superseded the call mid-assembly".
  - `Error` when "the wire promised a complete block". This covers Anthropic `content_block_stop` (`providers/anthropic/streaming.rs:448-450`), Gemini interactions, and OpenAI chat without a length finish.
- Incomplete tool calls are dropped only on a length finish. The non-streaming OpenAI-compatible path drops calls with empty or unparseable arguments only when `finish_reason` maps to `Length` (`providers/internal/openai_chat_completions_compatible.rs:70-128`).
- Tool-name repair is a hook decision, never automatic. `on_invalid_tool_call` returns `Fail`, `Retry{feedback}`, `Repair{tool_name}`, `Skip{reason}` or `Stop` (`crates/rig-agent/src/run/policy.rs:71-98`). The default is fail-fast (`crates/rig-agent/src/agent/hook.rs:1034-1045`). `Repair` applies to `UnknownTool` only: "Repair replaces a *name*; it cannot rewrite argument bytes, so a repair of malformed input would dispatch a tool with arguments the model never produced. Fail closed" (`crates/rig-agent/src/run/mod.rs:1228-1242`). A repaired name must be in `allowed_tool_names`. `Retry` is capped by `max_invalid_tool_call_retries` (`:1220-1226`).

Passes back to the model as errors:

- `MalformedToolInput` reaches the hook, or the caller, with `raw` "byte-for-byte as accumulated" and the parser `error` (`crates/rig-core/src/error.rs:150-171`). By default the run fails with "tool call `<name>` arrived with malformed JSON input: <err>" (`crates/rig-agent/src/run/mod.rs:112-120`). The model sees it only if a hook returns `Retry` or `Skip`. `InvalidToolCallContext` gives the hook `available_tools` and `allowed_tools` (`policy.rs:31-55`).

Content filters, repetition and leaked calls:

- Finish reasons `content_filter`, `safety`, `blocklist`, `prohibited_content` and `spii` map to `FinishReason::ContentFilter` (`openai_chat_completions_compatible.rs:63-65`).
- No repetition or leaked-call handling was found.

Logging:

- Raw text is kept exactly. The drop paths only `tracing::debug!`.

Configuration:

- Unparseable-argument handling is fixed per provider adapter. Name repair is left to the application's hook.

## Harness Playbook (omp author)

Verbatim, from "The Harness Playbook":

> RL-maxxed agents may call a familiar tool using another harness's schema. Composer models sometimes emit Grep with their expected shape even when no Grep tool exists. Codex may see paths: string[] and send one string delimited by ; or , , according to the mood of the day. The library should therefore validate and correct. Be strict about the tool's semantic contract, but charitable about the model's dialect: repair paths: "a,b" into a list when the mapping is unambiguous; otherwise return a structured, retryable error. A raw JSON Schema validator cannot own this layer by itself.

> Inference libraries also need to: repair malformed JSON; detect repetition loops in models such as Gemini and DeepSeek; parse each model's output dialect and synthesize canonical tool_call and think blocks when structured output leaks into text.

Its flowchart ends with: "Ship JSON Schema only; unconstrained sampling + charitable client-side repair … Repair client-side, surface structured error to model → Model retries with correction signal."

These are the author's design claims for omp². None of the four shipping agents above does leaked-call parsing or repetition detection.

## Comparison

| | pi | Codex | Claude Code | opencode v2 | rig |
|---|---|---|---|---|---|
| Malformed argument JSON | Repairs escapes, then partial-parse salvage, else `{}`; never an error | Error to model: "failed to parse function arguments: …" | Error to model with the first bytes echoed and common causes | Native routes: partial-parse salvage. AI SDK route: error to model | Per adapter: drop, `{}`, or typed error; default fails the run |
| Truncated (length) calls | All calls fail unexecuted with a re-issue message | Incomplete response becomes a stream error | "exceeded the N output token maximum" | Not special-cased | Dropped on a length finish only |
| Type coercion | TypeBox `Convert` plus its own coercion, always on | None (serde) | Per-tool `coerceInput`, then Zod | Schema-guided plugin, unambiguous cases only | None |
| Cross-harness dialect | `edit` only: stringified or single edits, `oldText`/`newText` | `apply_patch` heredoc and `applypatch`, for all models | Edit `old_str`/`new_str`/`path`, Read arrays and `length`, Bash `timeout_ms` | None beyond schema repair | None |
| Unknown tool name | "Tool X not found" | "unsupported call: X" | "No such tool available: X" | "No tool named X … use a tool from the available tool list" | Hook: fail, retry, repair name, skip, stop |
| Lists valid names | No | No | No | No (points to the list) | Gives them to the hook |
| Name aliasing or case | Case-insensitive on Anthropic OAuth only | None | Exact name or own `aliases` | None built in; hook may rewrite | Hook `Repair` only, must be an allowed name |
| Leaked calls in text | No | No | No | No | No |
| Repetition detection | No | No | No | No (v1 `doom_loop` key only) | No |
| Refusal or filter | Error stop, loop ends | Developer guidance injected, then retry | Fallback-model retry when armed | Not found | `ContentFilter` finish reason |
| What is logged | Repaired only; raw buffer deleted | Raw payload to telemetry | Coercion telemetry with `shapeClass`; raw truncated to 2 KiB | Raw on AI SDK error; salvaged otherwise | Raw byte-for-byte |
| Per model or provider | No | Filter guidance per model | No (per tool, one flag) | Per route | Per provider adapter |
