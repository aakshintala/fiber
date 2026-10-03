# The default cache-warming cap

Research for [#436](https://github.com/aakshintala/fiber/issues/436). The script is [warm_cap.py](warm_cap.py), which imports its session loaders from [ttl.py](ttl.py). The default it recommends is the owner's to accept.

## Question

What should `cache.warm_cap` default to, and does warming also need a savings threshold, as pi's `cacheWarming` has?

## Method

The script replays each session's requests as (context tokens, time) pairs, as ttl.py does, with the 1-hour lifetime L. Prices are multiples of base input price: a 1-hour write is 2.0, and a read is 0.05 on Opus 5.5 and 0.10 on Sonnet 5 (ttl.py's `READ`; the Claude prices come from pi's model data, `anthropic.json`, which matches the pricing table in README.md). The docs' round figure, 0.1, is shown as the 0.1 rows.

Warming at a cap of k lifetimes works like this.

- After each request the session refreshes just before each lifetime ends, once per lifetime, up to k refreshes. Each refresh reads the whole current context. The one output token is ignored.
- A request after a gap of g seconds finds the cache warm if g is at most (k + 1) × L. It then costs a read of the old context plus a write of the growth. Later than that it rewrites the whole context.
- Cap 0 is no warming, and reproduces ttl.py's 1-hour cost.
- The tail is counted for every session. After a session's last request nobody returns, so it spends all k refreshes on its last context.
- Refreshes are spaced exactly L apart. Pi refreshes at 0.9 × L, which would add about 11% more refreshes; this is not modelled.

Interactive sessions. I could not find the 2026-10-03 gap report in the repository, in issue comments or in /tmp, so the definition here is mine. A Claude Code session is interactive when its transcript says `entrypoint: cli` (a person at the terminal; `sdk-cli` is a script or delegate). A pi session is interactive when it has more than one user message (a scripted pi run sends one). By this definition 17% of interactive pi sessions (163) and 38% of interactive Claude Code sessions (214) contain a gap over one hour, close to the 20% and 41% in the ticket. If the report defines interactive differently, rerun the script with its definition in `_interactive_pi` and `_interactive_cc`.

Subagents are the Claude Code files under `/subagents/`, reported separately. A subagent counts as interactive when its own entries say `cli`, which means its parent was.

Sessions with a single request are dropped by the loaders, as in ttl.py. Each session is priced under both models, because the transcripts mix models (pi sessions include non-Claude models; the cost model treats them all as Anthropic).

Counts and token sizes only; no timings are reported.

## Data

| Source | Interactive sessions | All sessions | Gaps over 1 lifetime (interactive) | Gaps over 4 | Gaps over 12 | Last context p10 / p50 / p90 tokens (interactive) |
|---|---|---|---|---|---|---|
| pi main | 163 | 830 | 43 of 17,869 | 18 | 3 | 32,061 / 82,037 / 276,637 |
| Claude Code main | 214 | 296 | 125 of 21,834 | 40 | 7 | 96,540 / 182,516 / 341,097 |
| Claude Code subagents | 350 | 352 | 12 of 17,080 | 4 | 0 | 61,834 / 134,314 / 255,880 |

## Results

Total cost relative to no warming (1.000), by cap in lifetimes. Lower is better.


Interactive sessions

| Source | Read price | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 12 | 18 |
|---|---|---|---|---|---|---|---|---|---|---|
| pi main (163) | 0.05x | 1.000 | 0.982 | 0.984 | 0.983 | 0.984 | 0.987 | 0.993 | 1.013 | 1.046 |
| pi main (163) | 0.1x | 1.000 | 0.994 | 0.999 | 1.002 | 1.007 | 1.012 | 1.019 | 1.051 | 1.091 |
| Claude Code main (214) | 0.05x | 1.000 | 0.961 | 0.945 | 0.943 | 0.944 | 0.947 | 0.953 | 0.969 | 1.007 |
| Claude Code main (214) | 0.1x | 1.000 | 0.982 | 0.978 | 0.982 | 0.987 | 0.994 | 1.002 | 1.038 | 1.087 |
| Claude Code subagents (350) | 0.05x | 1.000 | 1.009 | 1.019 | 1.027 | 1.039 | 1.050 | 1.062 | 1.130 | 1.200 |
| Claude Code subagents (350) | 0.1x | 1.000 | 1.014 | 1.028 | 1.042 | 1.057 | 1.073 | 1.088 | 1.180 | 1.273 |

All sessions

| Source | Read price | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 12 | 18 |
|---|---|---|---|---|---|---|---|---|---|---|
| pi main (830) | 0.05x | 1.000 | 0.997 | 1.003 | 1.009 | 1.015 | 1.021 | 1.030 | 1.074 | 1.123 |
| pi main (830) | 0.1x | 1.000 | 1.004 | 1.013 | 1.022 | 1.031 | 1.040 | 1.051 | 1.108 | 1.169 |
| Claude Code main (296) | 0.05x | 1.000 | 0.963 | 0.948 | 0.947 | 0.948 | 0.952 | 0.958 | 0.977 | 1.017 |
| Claude Code main (296) | 0.1x | 1.000 | 0.984 | 0.980 | 0.984 | 0.990 | 0.997 | 1.006 | 1.046 | 1.097 |
| Claude Code subagents (352) | 0.05x | 1.000 | 1.009 | 1.019 | 1.027 | 1.039 | 1.050 | 1.062 | 1.130 | 1.201 |
| Claude Code subagents (352) | 0.1x | 1.000 | 1.014 | 1.028 | 1.042 | 1.057 | 1.073 | 1.088 | 1.180 | 1.274 |

Per session at a cap of 2 lifetimes (interactive), cost relative to that session with no warming:

| Source | Read price | p10 | p50 | p90 | Sessions where warming costs more |
|---|---|---|---|---|---|
| pi main | 0.05x | 1.006 | 1.025 | 1.042 | 91% |
| pi main | 0.1x | 1.007 | 1.035 | 1.073 | 91% |
| Claude Code main | 0.05x | 0.765 | 1.021 | 1.037 | 74% |
| Claude Code main | 0.1x | 0.857 | 1.026 | 1.060 | 74% |
| Claude Code subagents | 0.05x | 1.018 | 1.030 | 1.041 | 99% |
| Claude Code subagents | 0.1x | 1.023 | 1.042 | 1.069 | 99% |

Most sessions are never resumed, so most pay for the tail and lose a few percent. A minority are resumed after a pause, and each of those saves up to a quarter of its cost. The totals are small because those resumed sessions are few.

### Sensitivity to the read price

- At 0.05x the best cap is 1 lifetime for pi (0.982), 3 for Claude Code main (0.943), and none for subagents.
- At 0.1x the best cap is 1 for pi (0.994), 2 for Claude Code main (0.978), and none for subagents.
- At 0.1x, pi breaks even at 2 lifetimes and loses beyond; Claude Code main breaks even at 6.
- Across all sessions (mostly scripted runs) pi never gains more than 0.3%, and loses at 0.1x.
- The cap that is within 0.5 percentage points of the best, for interactive main sessions at either price, is 2 lifetimes. Longer caps help only Claude Code main at 0.05x, and by under 0.2 points.

## Savings threshold

Pi's rule, from `/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/dist/core/cache-warmer.js`: `expectedSavings = continuationProbability * missCost - warmCost`, and it warms when `expectedSavings >= CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS` (0.05 dollars). `continuationProbability` is 0.15 while idle (`IDLE_CONTINUATION_PROBABILITY`, "measured from our own usage"), `missCost` is a rebuild minus a read, and `warmCost` is a read plus one output token. The docs say the same: "Pi estimates at least $0.05 in avoided cache-miss cost" (`docs/settings.md`, line 21). Pi also stops idle warming after 30 minutes (`MAX_IDLE_WARMING_AGE_MS`).

I modelled two thresholds, both converted to tokens with base input prices of 4 dollars per million (Opus 5.5) and 2 (Sonnet 5), from pi's `anthropic.json`, with a 1-hour rebuild of 2.0. A session's context is fixed while idle, so the threshold is a context size below which a gap is not warmed.

| Rule | Opus 5.5 | Sonnet 5 |
|---|---|---|
| Rebuild minus read is at least 0.05 dollars | 6,411 tokens | 13,158 tokens |
| Pi's rule: 0.15 × (rebuild minus read) minus read is at least 0.05 dollars | 51,547 tokens | 135,136 tokens |

The first rule excludes almost nothing (under 2% of sessions end below it) and changes no total by more than 0.001. Pi's full rule, at a cap of 2 lifetimes, compared with none:

| Source | Model | Total without | Total with | Sessions costing more, without | with |
|---|---|---|---|---|---|
| pi main, interactive | Opus 5.5 | 0.984 | 0.983 | 91% | 63% |
| pi main, interactive | Sonnet 5 | 0.999 | 0.999 | 91% | 31% |
| Claude Code main, interactive | Opus 5.5 | 0.945 | 0.945 | 74% | 73% |
| Claude Code main, interactive | Sonnet 5 | 0.978 | 0.986 | 74% | 59% |
| Claude Code subagents, interactive | Opus 5.5 | 1.019 | 1.019 | 99% | 94% |
| Claude Code subagents, interactive | Sonnet 5 | 1.028 | 1.021 | 99% | 48% |

The threshold barely moves the total. It removes losing sessions (the fraction costing more falls) but also removes small winners, so on Claude Code main with Sonnet 5 the total gets worse (0.978 to 0.986). It earns its complexity only for subagents, where the best answer is to not warm at all.

## Recommendation

Default `cache.warm_cap` to 2 lifetimes, which is `"2h"` with the 1-hour lifetime. The owner decides whether to accept it.

- It is at or within 0.5 points of the best cap for interactive main sessions at both read prices (pi 0.984 and 0.999; Claude Code main 0.945 and 0.978).
- Its worst case on a never-resumed session is 2 refreshes, 0.1 to 0.2 times the context, against a rebuild of 2.0.
- The gain is small: 1.6% to 5.5% of spend on interactive main sessions at best, and nothing at 0.1x for pi. Warming stays off by default, so this cap only matters to someone who turns it on.
- Do not add a savings threshold. It does not change the totals meaningfully and adds a rule, a probability constant and two prices to maintain. If one is wanted, use a plain minimum context of about 50,000 tokens rather than pi's probability model.
- Warming should not apply to subagents: they lose at every cap (1.009 to 1.274), because they almost never wait over an hour. `docs/prompt-cache.md` already says a delegate never warms.

## Caveats

- Costs ignore the one output token per refresh, which at 20 dollars per million is under 0.00002 dollars each.
- The warm result assumes a refresh reads the cache and resets its lifetime. README.md quotes that a read refreshes the lifetime to the same lifetime.
- The sample is one person's sessions: 163 interactive pi sessions, 214 Claude Code. The gain comes from a small number of resumed sessions (43 and 125 gaps over an hour), so the best cap between 1 and 4 is within noise. The choice of 2 is a judgement within a flat region.
- Transcripts do not record which model priced each session, so both models are applied to all. Pi sessions that used non-Claude models are priced as if on Anthropic.
- Sessions still open when the data was collected are counted as ended; their tail is charged.
- Handoffs and compaction are ignored, as in ttl.py.

## Draft doc edits

Replacement for `docs/prompt-cache.md` lines 181-185 (the paragraph beginning "The cap has a ceiling"):

```
The cap has a ceiling. A refresh costs 0.05 to 0.1 times the prompt's input
price and a 1-hour rebuild 2 times, so warming through N lifetimes on a session
nobody returns to wastes at most 0.1 × N. Below 19 lifetimes, that is always
less than the one rebuild warming guards against. The default cap is 2
lifetimes, `"2h"`. Replayed on the owner's interactive sessions with their real
gaps, a cap of 1 to 3 lifetimes cuts spend by 2 to 6 percent on Opus 5.5 and
by 0.1 to 2 percent on Sonnet 5, and longer caps give most of it back as
refreshes to sessions nobody resumed. A subagent session loses at every cap.
Fiber has no savings threshold: one changes no total by more than a percentage
point ([research/prompt-cache/warm-cap.md](../research/prompt-cache/warm-cap.md)).
```

Replacement for `docs/configuration.md` line 110:

```
| `cache.warm_cap` | `"2h"` | yes | How long after the last turn warming stops, as a duration such as `"4h"`; never more than 19 cache lifetimes (`docs/prompt-cache.md`, "Warming while idle"). |
```
