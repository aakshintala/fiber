# Databricks gateway probe (#4610)

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
