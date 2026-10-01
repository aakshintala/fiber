# The session log is the only state of record

Fiber's predecessor kept an append-only event log and, beside it, four files
derived from that log: a session manifest with token totals and a checkpoint
hash, a usage ledger, a commit manifest and an authority marker. They could and
did disagree — 63 sessions ended up flagged `projection_invalid` from stale
manifests, and `ask` could exit with an indeterminate commit status. The same
failure was found independently in another harness: of 78 official Pi extensions
audited, 17 held state and 2 did it correctly, the rest keeping it in closures,
live maps or a rescan on restore, which is why rewind and resume lie there.

So: **the log is the only authority** for what happened in a session.
Anything the loop, the TUI or an extension needs to know about a session
after a resume is an event or a fold of events; runtime objects may cache and
index but never become a second truth. What describes no single session
lives in Fiber home instead: configuration, credentials, and an extension's
data directories (`docs/state.md`). The log is append-only for
the life of the session, so a handoff appends new events rather than rewriting
it, and a session directory holds no file that can contradict it. The contract
is `docs/events.md`; the rationale and the rejected alternatives are on
[issue #6](https://github.com/aakshintala/fiber/issues/6).

## Consequences

- Every fact a consumer needs about a session must be an event. When the TUI
  or an extension wants state the contract does not carry, the fix is a new
  event kind, never a side channel — and a new kind is additive, so this is
  cheap on purpose. An extension's own state in a session is such a kind,
  `extension_state_set` (`docs/extensions.md`, "State"), never a file beside
  the log and never Lua globals.
- Opening a session folds the log. That cost is bounded by indexing line offsets
  and parsing only the window a consumer asks for, not by caching the fold to
  disk. If folding ever becomes too slow, the answer is a rebuildable derived
  index that is explicitly not the truth, never a sidecar of record.
- Totals that used to be stored — tokens, cost, attempt counts, history length —
  are computed, so a late correction is a new event that the fold absorbs
  rather than a reconciliation pass over two stores.
- A parent's log holds a copy of each `usage_recorded` its delegates sent it,
  naming the delegate in `origin_session_id`. The copy is the parent's own
  record of what its tree spent, not a second record of the delegate's call:
  the delegate's log remains the authority for the delegate, and a fold counts
  one line per `generation_id`, so the copies never double count
  (`docs/events.md`, `usage_recorded`).
- `recent.jsonl` and the MCP tool-list cache in Fiber home are rebuildable
  derived indexes, explicitly not the truth (`docs/state.md`).
