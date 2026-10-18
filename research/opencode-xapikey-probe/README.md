# OpenCode `x-api-key` probe

Question: does `POST https://opencode.ai/zen/go/v1/messages` accept the
`x-api-key` header for auth, instead of an `Authorization` bearer header?

## Method

`probe.sh` on October 18, 2026, from macOS. The key is read from
`~/.config/probe-keys/opencode-key` into an env var inside the script; it is
never echoed, logged, or written to a file, and neither this README nor
`raw/messages.json` carries it. One request only, per the ticket.

Model: `claude-haiku-5-5`, the first model in
`providers/opencode/providers/opencode-go.json` whose protocol is
`anthropic-messages`. One POST to
`https://opencode.ai/zen/go/v1/messages` with headers
`x-api-key: $KEY`, `anthropic-version: 2023-06-01`,
`content-type: application/json`, and no `Authorization` header. Body:
`{"model":"claude-haiku-5-5","max_tokens":16,"messages":[{"role":"user","content":"Reply with the word ok."}]}`.

## Result: FAIL (HTTP 400, before auth was evaluated)

The request answered HTTP 400 with this body (`raw/messages.json`):

```json
{"type":"error","error":{"type":"MissingSessionID","message":"Request is missing x-opencode-session and cannot be routed efficiently. Please see https://opencode.ai/docs/go/#where-can-i-use-it"}}
```

Go rejected the request for the missing `x-opencode-session` header, which
the ticket's header list omits, so the request never reached auth: the probe
is inconclusive on whether `x-api-key` authenticates. A 401 or 403 would have
been an auth verdict; this 400 is not one. Usage fields are absent from the
error body (there is no 200 body to check for them).

No code was changed: the fix needs a fence widening outside this probe.

## Doc check

`docs/model-routing.md` says nothing about the auth header: line 91 names
only the credential source ("key, one for both"), and lines 203-240 (the
OpenCode wire facts) name request headers only for `x-opencode-session` and
`User-Agent`. Left unchanged.
