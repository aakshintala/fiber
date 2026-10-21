# Cost and quota

A subscription bills nothing per call. Fiber still logs each subscription call with `subscription`, so you see what the work would cost on an API key, kept apart from money billed per token — and `budget.usd` never counts those calls. The exact rule is [Cost](../model-routing.md) and [Spending budget](../loop.md).

## What each provider reports

Where quota comes from, and what the model sees, is [Quota](../model-routing.md) and [Provider quota](../tools.md). For the friends release, codex and opencode-go report percent used per window with reset times; muse reports none. Beyond the friends release, OpenRouter reports credit remaining and the limit for a key with a limit; a key without one reports none.

When a window crosses `quota.notice_at` percent used (default 80), one line naming the provider, the label, the window and its reset time goes into the model's next turn input.

## Seeing it yourself

`/quota` opens the quota view and fetches every label's quota ([Swapped views](../tui.md)). The same answer is available to drivers as the `quota` command, which answers from the cache without `fetch` and refetches every label with `fetch: true` ([What each command does](../invocation.md)).
