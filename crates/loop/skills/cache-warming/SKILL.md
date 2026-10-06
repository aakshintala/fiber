---
name: cache-warming
description: Recommend a value for cache.warm_cap, and whether to turn on cache.warm_idle, by replaying the person's own Fiber sessions against the prompt cache's prices.
---

# Recommending cache.warm_cap

`cache.warm_idle` makes an idle session refresh its prompt cache shortly before the cache lifetime ends, so a person who returns after a long pause finds it warm. `cache.warm_cap` is how long after the last turn warming stops. Warming costs a little on every session nobody returns to and saves a lot on one that is resumed after a pause. This recipe works out which cap would have cost least on the person's own sessions. Background: `docs/prompt-cache.md`, "Warming while idle". Read it first. The method and the owner's figures are in `research/prompt-cache/warm-cap.md` in the Fiber repository, when it is available.

Use your ordinary tools: read the logs, write a short script in a scratch directory and run it. Add no tool and change no Fiber file.

## 1. Collect the data

Session logs are `projects/<key>/sessions/<id>/events.jsonl` in Fiber home (`~/.fiber` unless moved; `docs/state.md`). Each line is a JSON object with a `kind` and a `ts` in milliseconds (`docs/events.md`).

- Keep an interactive session: one with more than one `turn_started`, and no `parent` in its `session_started` payload. A `fiber ask` run, or a delegate, never warms, so leave them out.
- For each kept session, take every `usage_recorded` line in order. Its request time is the line's `ts`. Its context size is `tokens.input + tokens.cache_read` plus the sum of `tokens.cache_write`.
- Drop a session with fewer than two requests.
- The cache lifetime L is 1 hour unless the person set another (`docs/prompt-cache.md`, "Cache lifetime"). Use the lifetime the sessions ran with.

## 2. Price each cap

Prices are multiples of the model's base input price: a refresh or any cache read is 0.1 (0.05 on some models; use the model's own cache-read price when it has one), and a 1-hour write is 2.0. Replay every session at caps of 0 to 6 lifetimes. A cap of k lifetimes means:

- After each request the session refreshes just before each lifetime ends, once per lifetime, up to k refreshes. Each refresh reads the whole current context.
- A request after a gap g finds the cache warm when g is at most (k + 1) x L. It costs a read of the old context plus a write of the growth over it. After a longer gap it rewrites the whole context at the write price.
- Cap 0 is no warming. It is the baseline.
- Count the tail for every session. After its last request nobody returns, so it spends all k refreshes on its last context.

Sum the cost over all kept sessions for each cap and divide by the cap-0 sum.

## 3. Recommend

- Take the smallest cap whose total is within 0.5 percentage points of the lowest total.
- When no cap beats 0, recommend leaving `cache.warm_idle` off.
- The cap must stay under 19 lifetimes; Fiber refuses 19 or more with `config_invalid` (`docs/configuration.md`).

Report the number of sessions and requests used, the table of totals by cap, and the recommendation. The result is a count over the person's own history and not a promise: say how few resumed sessions carry the saving. With fewer than about 20 interactive sessions, say the sample is too small to recommend.

To apply it, with the person's agreement:

```
fiber config set cache.warm_idle true
fiber config set cache.warm_cap 2h
```

Replace `2h` with the recommended number of lifetimes in hours. `fiber config set` writes the global file; `--repo` and the per-project forms are in `docs/configuration.md`.
