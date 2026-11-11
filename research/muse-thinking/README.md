# Muse thinking-level probes

Question: which thinking levels do `muse-ember-1.0` and `muse-wonder-1.0`
take on the Meta route (#4614)?

## Method

One tiny `responses` request per thinking level (`off`, `minimal`, `low`,
`medium`, `high`, `xhigh`, `max`) per model against
`https://api.meta.ai/v1/responses`, shaped the way Fiber sends that level
on the protocol: `reasoning.effort` is the level, or `"none"` for `off`;
`max_output_tokens: 16`. One request per level, no retry.

The key travels in the `Authorization` header, read from
`~/.config/probe-keys/muse-key`; it is never printed, never on a command
line, and every record is checked for it before writing. A dropped
connection would be recorded as a transport error, not retried; none
dropped in this run.

"Accepts" means the route answered 200 to that level's wire form. The
route validates per model: the `off` rejection names the supported values
(`reasoning_effort 'none' is not supported for model ... Supported
values: [minimal, low, medium, high, xhigh, max]`), so a bare 200 is not
the route ignoring the field.

## Findings

| Model | Accepts | Record |
| --- | --- | --- |
| `muse-ember-1.0` | `minimal`–`max` | `raw/muse-ember-1.0.json` (`off` 400 names the supported set) |
| `muse-wonder-1.0` | `minimal`–`max` | `raw/muse-wonder-1.0.json` (`off` 400 names the supported set) |

## Consequence

Both ids take `minimal` through `max` with no model-declared default, so
their `MUSE` thinking rows use that level set with a `None` default, the
same shape as the `muse-spark-1.3` rows.
