# Cost and quota

A subscription bills nothing per call. Fiber still logs each subscription call with `subscription`, so you see what the work would cost on an API key, kept apart from money billed per token — and `budget.usd` never counts those calls. The exact rule is [Cost](../model-routing.md) (`docs/model-routing.md`, "Cost") and [Spending budget](../loop.md) (`docs/loop.md`, "Spending budget").

## What each provider reports

Where quota comes from, and what the model sees, is [Quota](../model-routing.md) (`docs/model-routing.md`, "Quota") and [Provider quota](../tools.md) (`docs/tools.md`, "Provider quota"). For the friends release:

- **codex** reports percent used of a primary and a secondary window, with reset times, from `/wham/usage`.
- **opencode-go** reports percent used of Go's rolling, weekly and monthly windows, with reset times, from `GET /zen/go/v1/usage`. Zen reports none.
- **muse** reports none: no quota reported.

When a window crosses `quota.notice_at` percent used (default 80), one line naming the provider, the label, the window and its reset time goes into the model's next turn input.

## Seeing it yourself

`/quota` opens the quota view and fetches every label's quota ([Swapped views](../tui.md) (`docs/tui.md`, "Swapped views")). The same answer is available to drivers as the `quota` command, which answers from the cache without `fetch` and refetches every label with `fetch: true` ([What each command does](../invocation.md) (`docs/invocation.md`, "What each command does")).
