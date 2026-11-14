# Databricks gateway probe (#4610, #4616)

Question: on Databricks' chat-completions gateway route, does
`gemini-4-argon` accept `reasoning_effort`, and does `gemini-3-8-flash`?

## Method

No live call was made for this record. It transcribes the owner's
2026-11-11 probe as reported in [the owner's probe
comment](https://github.com/aakshintala/fiber/issues/4610#issuecomment-6783297010).
There are no `raw/` bodies: nothing was captured here, so there is nothing
to save.

## Results

Quoting the operative lines:

> **Confirmed by the owner's probe (2026-11-11).** On the same
> chat-completions route, `gemini-4-argon` answers when the request carries
> no `reasoning_effort`, and returns the 400 when it carries
> `reasoning_effort: "medium"`. With the same field, `gemini-3-8-flash`
> answers. It fails at both medium and high, so the value isn't the cause.
> The fix stands: no thinking levels for argon on Databricks. Argon still
> reasons at its own default.

Scoped to what the comment reports: one gateway route, two models, the
owner's workspace, 2026-11-11. `gemini-4-argon` answers 200 with no
`reasoning_effort` and 400s with `medium` and `high`; `gemini-3-8-flash`
answers 200 with the field. No other model, route, or field was measured
for this record.

## `max_completion_tokens` on open-weight routes (#4616)

Question: on Databricks' chat route, do the open-weight chat services
accept `max_completion_tokens`, and do the GPT and Gemini services on the
same route?

## Method

No live call was made for this record. It transcribes the owner's
2026-11-11 probe as reported in [issue
#4616](https://github.com/aakshintala/fiber/issues/4616), at `da1cce64`.
There are no `raw/` bodies: nothing was captured here, so there is nothing
to save.

## Results

Quoting the operative lines:

> **Confirmed by the owner's probe (2026-11-11, at `da1cce64`).** On
> Databricks, the open-weight chat services reject `max_completion_tokens`
> with HTTP 400 `json: unknown field "max_completion_tokens"`: `gpt-oss-20b`,
> `gpt-oss-120b`, `gemma-3-12b`, `llama-4-maverick`,
> `meta-llama-3-1-8b-instruct`, `meta-llama-3-3-70b-instruct` and
> `qwen3-next-80b-a3b-instruct`. A direct probe on `gpt-oss-20b`:
> `max_completion_tokens` gets the 400, while `max_tokens` answers, with or
> without `reasoning_effort: "low"`. Every GPT-5/6 and Gemini service on the
> same route accepts the field.

Scoped to what the issue reports: one Databricks route, the seven listed
open-weight services rejecting `max_completion_tokens` with a 400, the
`gpt-oss-20b` direct probe answering on `max_tokens` with and without
`reasoning_effort: "low"`, and GPT-5/6 and Gemini services accepting
`max_completion_tokens`, in the owner's workspace, 2026-11-11. No other
model, route, or field was measured for this record.
