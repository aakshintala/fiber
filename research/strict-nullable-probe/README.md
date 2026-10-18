# Required nullable tool arguments

The recorded calls accepted required nullable scalar properties. This supports
using nullable arguments without rewriting tool schemas for strict mode.
These are the orchestrator's approved probe records, not calls run by the
implementation lane.

## Request and limits

Every request declared a `read` tool with this schema:

```json
{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":["integer","null"]},"note":{"type":["string","null"]},"all":{"type":["boolean","null"]}},"required":["path","offset","note","all"],"additionalProperties":false}
```

The P1 through P5b prompt was: “Call the read tool with path 'a.txt'. Pass null
for every other argument.” This tests acceptance and explicit null output, not whether
a model spontaneously chooses null for an unspecified argument. Each successful
call returned `{"path":"a.txt","offset":null,"note":null,"all":null}`
(disregarding key order and whitespace).

Only integer, string and boolean nullable unions were exercised. Nullable
number, object and array types are not covered. These samples do not establish
strict enforcement against adversarial prompts or acceptance by every endpoint
that speaks these protocols.

## Results

| Record | Endpoint and model | HTTP | Setting |
|---|---|---|---|
| [P1](raw/P1_openai_responses.json) | OpenAI Responses, `gpt-6-luna` | 200 | `strict: true` |
| [P2](raw/P2_openai_chat.json) | OpenAI Chat Completions, `gpt-6-luna` | 200 | `strict: true`, `reasoning_effort: none` |
| [P3](raw/P3_anthropic_messages.json) | Anthropic Messages, `claude-sonnet-5-5` | 200 | `strict: true`, `tool_choice: auto` |
| [P4 AUTO](raw/P4_gemini_AUTO.json) | Gemini, `gemini-3-flash-preview` | 200 | `AUTO` |
| [P4 VALIDATED](raw/P4_gemini_VALIDATED.json) | Gemini, `gemini-3-flash-preview` | 200 | `VALIDATED` |
| [P5](raw/P5_opencode_go_completions.json) | OpenCode Go Chat Completions, `glm-5.3-flash` | 200 | `strict: true` |
| [P5b](raw/P5b_openrouter_anthropic_messages.json) | OpenRouter Messages, `anthropic/claude-haiku-4.5` | 200 | `strict: true`, `tool_choice: auto` |

P1's response echoes the schema unchanged and `strict: true`. The other
responses do not echo the declaration: their raw records retain the sent
schema and strict setting or Gemini mode. P5b reports Amazon Bedrock as its
upstream provider. It is the OpenRouter Messages compatibility sample, not a
direct Bedrock probe.

## Path-only omission case

These calls test the omission case: the prompt names only `path`, without
asking for nulls or naming the other arguments. Each request uses the schema
above with `strict: true`. Each model sends null for every unspecified argument.
The owner accepted this evidence under ruling A on October 18, 2026.

| Call | Vendor and model | HTTP | Prompt | Arguments sent |
|---|---|---|---|---|
| [A1](raw/A1_openai_responses_pathonly.json) | OpenAI Responses, `gpt-6-luna` | 200 | Call the read tool with path 'a.txt'. | `{"path":"a.txt","offset":null,"note":null,"all":null}` |
| [A2](raw/A2_openai_chat_pathonly.json) | OpenAI Chat Completions, `gpt-6-luna` | 200 | Call the read tool with path 'a.txt'. | `{"path":"a.txt","offset":null,"note":null,"all":null}` |
| [A3](raw/A3_anthropic_messages_pathonly.json) | Anthropic Messages, `claude-sonnet-5-5` | 200 | Call the read tool with path 'a.txt'. | `{"path":"a.txt","offset":null,"note":null,"all":null}` |

## Malformed first requests

The raw evidence retains 3 initial HTTP 400 responses. None reports rejection
of the nullable schema:

- [P2 first request](raw/P2_openai_chat_attempt1_request-shape-error.json)
  omitted `reasoning_effort: none`, required for this model's Chat Completions tools
- [P3 first request](raw/P3_anthropic_messages_attempt1_request-shape-error.json)
  used unsupported `tool_choice: any`
- [P5 first request](raw/P5_opencode_go_completions_attempt1_request-shape-error.json)
  lacked the routing header `x-opencode-session`

Probe scripts are not included. The raw records preserve the requests and
responses without authentication headers.
