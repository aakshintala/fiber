# Native tool search forms, quoted and probed

The wire shapes `anthropic-messages` and `openai-responses` use for deferred
tools and for the result of Fiber's own `tool_search` (#368). The vendor
quotes were fetched 2026-10-17. The probes ran the same day.

## Anthropic

Source: https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool,
"Custom tool search implementation" (fetched 2026-10-17).

- "return a standard `tool_result` with `tool_reference` blocks in the content array: `{"type": "tool_result", "tool_use_id": "toolu_your_tool_id", "content": [{ "type": "tool_reference", "tool_name": "discovered_tool_name" }]}`"
- "Every tool referenced must have a corresponding tool definition in the top-level `tools` parameter, normally with `defer_loading: true`."
- "At least one tool, normally the tool search tool itself, must stay non-deferred."
- "Never set `defer_loading: true` on the tool search tool itself."
- "A tool with `defer_loading: true` can't also carry `cache_control`: the API returns a 400."

## OpenAI

Source: https://developers.openai.com/api/docs/guides/tools-tool-search,
"Client-executed tool search" (fetched 2026-10-17).

- "Configure the `tool_search` tool with `execution: "client"` and a schema for the search arguments your application expects"
- The call item: `{"type": "tool_search_call", "execution": "client", "call_id": "call_abc123", "status": "completed", "arguments": {...}}`
- The result item: `{"type": "tool_search_output", "execution": "client", "call_id": "call_abc123", "status": "completed", "tools": [{"type": "function", "name": ..., "description": ..., "defer_loading": true, "parameters": {...}}]}`
- "Echo the same `call_id` from the `tool_search_call` in your `tool_search_output`."

## Probes, 2026-10-17

Owner-approved live probes, recorded in
https://github.com/aakshintala/fiber/issues/368#issuecomment-6246237517.
All four requests returned HTTP 200. Cost was under US$0.01 in total (about
1,400 input and 210 output tokens). Response summaries omit thinking
signatures.

### A1 (Anthropic, `claude-haiku-5-5`)

`tool_reference` for an already-loaded tool, `cache_control` on the
`tool_result`. HTTP 200. Request:

```json
{"model":"claude-haiku-5-5","max_tokens":64,"tools":[{"name":"tool_search","description":"Search deferred tools by keyword.","input_schema":{"type":"object","properties":{"query":{"type":"string","minLength":1},"limit":{"type":"integer","minimum":1}},"required":["query"]}},{"name":"alpha","description":"Tool alpha.","input_schema":{"type":"object","properties":{"x":{"type":"string"}}},"defer_loading":true},{"name":"bravo","description":"Tool bravo.","input_schema":{"type":"object","properties":{"x":{"type":"string"}}},"defer_loading":true}],"messages":[{"role":"user","content":"Find alpha and bravo."},{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"tool_search","input":{"query":"alpha bravo"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"tool_reference","tool_name":"alpha"},{"type":"tool_reference","tool_name":"bravo"}]}]},{"role":"assistant","content":[{"type":"tool_use","id":"toolu_2","name":"tool_search","input":{"query":"alpha"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_2","content":[{"type":"tool_reference","tool_name":"alpha"}],"cache_control":{"type":"ephemeral"}}]}]}
```

Response:

```json
{"stop_reason": "max_tokens", "usage": {"input_tokens": 2, "cache_creation_input_tokens": 814, "output_tokens": 64}, "content_types": ["thinking"]}
```

### A2 (Anthropic, `claude-haiku-5-5`)

History holds `tool_search` calls, `tool_search` not declared, results as
text. HTTP 200. Request:

```json
{"model":"claude-haiku-5-5","max_tokens":64,"tools":[{"name":"alpha","description":"Tool alpha.","input_schema":{"type":"object","properties":{"x":{"type":"string"}}}},{"name":"bravo","description":"Tool bravo.","input_schema":{"type":"object","properties":{"x":{"type":"string"}}}}],"messages":[{"role":"user","content":"Find alpha and bravo."},{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"tool_search","input":{"query":"alpha bravo"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"Loaded: alpha, bravo."}]},{"role":"assistant","content":[{"type":"tool_use","id":"toolu_2","name":"tool_search","input":{"query":"alpha"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_2","content":"Loaded: alpha."}]}]}
```

Response:

```json
{"stop_reason": "max_tokens", "usage": {"input_tokens": 559, "cache_creation_input_tokens": 0, "output_tokens": 64}, "content_types": ["thinking"]}
```

### B1 (OpenAI Responses, `gpt-6-luna`)

`tool_search_output` naming an already-loaded tool. HTTP 200. Request:

```json
{"model":"gpt-6-luna","max_output_tokens":64,"store":false,"tools":[{"type":"tool_search","execution":"client","description":"Search deferred tools by keyword.","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}},{"type":"function","name":"alpha","description":"Tool alpha.","parameters":{"type":"object","properties":{"x":{"type":"string"}}},"defer_loading":true},{"type":"function","name":"bravo","description":"Tool bravo.","parameters":{"type":"object","properties":{"x":{"type":"string"}}},"defer_loading":true}],"input":[{"role":"user","content":"Find alpha and bravo."},{"type":"tool_search_call","execution":"client","call_id":"call_1","status":"completed","arguments":{"query":"alpha bravo"}},{"type":"tool_search_output","execution":"client","call_id":"call_1","status":"completed","tools":[{"type":"function","name":"alpha","description":"Tool alpha.","parameters":{"type":"object","properties":{"x":{"type":"string"}}},"defer_loading":true},{"type":"function","name":"bravo","description":"Tool bravo.","parameters":{"type":"object","properties":{"x":{"type":"string"}}},"defer_loading":true}]},{"type":"tool_search_call","execution":"client","call_id":"call_2","status":"completed","arguments":{"query":"alpha"}},{"type":"tool_search_output","execution":"client","call_id":"call_2","status":"completed","tools":[{"type":"function","name":"alpha","description":"Tool alpha.","parameters":{"type":"object","properties":{"x":{"type":"string"}}},"defer_loading":true}]}]}
```

Response:

```json
{"status": "completed", "incomplete_details": null, "usage": {"input_tokens": 382, "output_tokens": 17, "total_tokens": 399}, "output_types": ["function_call"]}
```

### B2 (OpenAI Responses, `gpt-6-luna`)

Zero-match `function_call` named `tool_search` plus `function_call_output`.
HTTP 200. Request:

```json
{"model":"gpt-6-luna","max_output_tokens":64,"store":false,"tools":[{"type":"tool_search","execution":"client","description":"Search deferred tools by keyword.","parameters":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}},{"type":"function","name":"alpha","description":"Tool alpha.","parameters":{"type":"object","properties":{"x":{"type":"string"}}},"defer_loading":true},{"type":"function","name":"bravo","description":"Tool bravo.","parameters":{"type":"object","properties":{"x":{"type":"string"}}},"defer_loading":true}],"input":[{"role":"user","content":"Find alpha and bravo."},{"type":"function_call","call_id":"call_3","name":"tool_search","arguments":"{\"query\":\"?!\"}"},{"type":"function_call_output","call_id":"call_3","output":"No deferred tool matched the query."}]}
```

Response:

```json
{"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}, "usage": {"input_tokens": 86, "output_tokens": 64, "total_tokens": 150}, "output_types": ["reasoning"]}
```

B2 ended `status: incomplete` because the 64-token cap was spent on
reasoning. The request was accepted (200), which is what the probe tests.

### Result

No vendor refused. The zero-match form and the shapes for calls to a tool the
current preamble lacks are accepted as specified. No probe sent `strict: true`
together with `defer_loading: true` on Anthropic, nor a `strict` key on a
`tool_search_output` tool on OpenAI.
