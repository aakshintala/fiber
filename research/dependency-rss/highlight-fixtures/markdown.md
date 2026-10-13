# Session search

Searches past sessions (`docs/tools.md`, "Searching past sessions"). Two
modes: list matching sessions, or show one session's turns.

## Query syntax

- Plain words match all of them (`AND`).
- `"quoted phrases"` match exactly.
- `from:2026-10-01` bounds the date; `tool:edit` names a tool.

```bash
fiber session search "release size" --limit 5
fiber session search tool:edit from:2026-10-01
```

## Result shape

| Field     | Meaning                        |
|-----------|--------------------------------|
| `id`      | short session id               |
| `title`   | first user line, truncated     |
| `matches` | one line of context per hit    |

1. Results stream newest first.
2. A session with no hits never appears.
3. `--limit 0` means no limit.

> Deleted sessions are gone: search reads the log dir, and there is no
> separate index to go stale.

See also: [state](state.md), [tools](tools.md#search).

## Budget

Highlighting one block in each of three languages is the documented
figure; per-language rows come from the `arborium highlighting` table.
