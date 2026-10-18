# OpenCode `x-api-key` probe

Question: does `POST https://opencode.ai/zen/go/v1/messages` accept the
`x-api-key` header for auth, instead of an `Authorization` bearer header?

## Method

`probe.sh` on October 18, 2026, from macOS. The key is read from
`~/.config/probe-keys/opencode-key` into an env var inside the script; it is
never echoed, logged, or written to a file, and neither this README nor
any raw file carries it. Two requests in total: one per the ticket, one per
the flywheel ruling on #1642.

Model: `claude-haiku-5-5`, the first model in
`providers/opencode/providers/opencode-go.json` whose protocol is
`anthropic-messages`. One POST to
`https://opencode.ai/zen/go/v1/messages` with headers
`x-api-key: $KEY`, `anthropic-version: 2023-06-01`,
`content-type: application/json`, and no `Authorization` header. Body:
`{"model":"claude-haiku-5-5","max_tokens":16,"messages":[{"role":"user","content":"Reply with the word ok."}]}`.

## Result: PASS (HTTP 200 with x-api-key alone)

First attempt: HTTP 400 (see below), rejected before auth was evaluated.
Second attempt, allowed by the flywheel ruling on #1642: the same request
with one header added, `x-opencode-session` holding a fresh uuid generated
per run (the package already sends that header on every `opencode-go`
request via `cache_key_header`; the ticket's header list simply omitted it).
Still no `Authorization` header. Raw body in `raw/messages-x-session.json`.

The second request answered HTTP 200 with the standard `anthropic-messages`
shape, in aggregate: `type: message`, `role: assistant`,
`stop_reason: end_turn`, one text block replying `ok`, `model` echoing the
requested id. Usage fields are present (`input_tokens`, `output_tokens`,
plus cache/thinking detail keys). No cost fields, no account data.

Verdict: `x-api-key` authenticates Go's `/v1/messages` alone. No code
change; `docs/model-routing.md` stays as checked below.

## First attempt record: FAIL (HTTP 400, before auth was evaluated)

The request answered HTTP 400 with this body (`raw/messages.json`):

```json
{"type":"error","error":{"type":"MissingSessionID","message":"Request is missing x-opencode-session and cannot be routed efficiently. Please see https://opencode.ai/docs/go/#where-can-i-use-it"}}
```

Go rejected the request for the missing `x-opencode-session` header, which
the ticket's header list omits, so the request never reached auth: the probe
is inconclusive on whether `x-api-key` authenticates. A 401 or 403 would have
been an auth verdict; this 400 is not one, which is why the ruling allowed
the second attempt with the session header added.

## Doc check

`docs/model-routing.md` says nothing about the auth header: line 91 names
only the credential source ("key, one for both"), and lines 203-240 (the
OpenCode wire facts) name request headers only for `x-opencode-session` and
`User-Agent`. Left unchanged.
