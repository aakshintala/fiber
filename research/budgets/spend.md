# Owner spend: pi vs Claude Code (main + subagent)

Generated 2026-09-30T00:52:25.545414Z by `spend.py`. Claude Code cost is API-equivalent (computed from tokens x price table); pi cost is the `usage.cost.total` the harness already logged, in USD, regardless of whether the underlying provider is a subscription login or an API key (see per-provider table below).

### pi

Sessions: **692**

**1. Per-session cost / tokens**

| | p50 | p90 | p99 | max |
|---|---|---|---|---|
| cost (USD) | 0.0546 | 0.9304 | 7.1793 | 16.7276 |
| tokens | 930,370 | 16,365,866 | 105,398,611 | 451,430,875 |

**2. Per-turn cost (USD)**

n=1465  p50=0.0174  p90=0.4535  p99=3.3317  max=12.4807

**4. Model calls per turn (steps per turn)**

n=1465  p50=8.0  p90=72.6  p99=220.8  max=939

### Claude Code main

Sessions: **292**
(unpriced/unknown-model assistant calls excluded from cost: 39)

**1. Per-session cost / tokens**

| | p50 | p90 | p99 | max |
|---|---|---|---|---|
| cost (USD) | 3.8816 | 21.6720 | 100.5317 | 195.2129 |
| tokens | 3,690,621 | 29,106,945 | 145,285,054 | 293,408,734 |

**2. Per-turn cost (USD)**

n=2732  p50=0.5297  p90=2.1011  p99=5.9168  max=17.8222

**4. Model calls per turn (steps per turn)**

n=2732  p50=4.0  p90=16.0  p99=44.7  max=105

### Claude Code subagent

Sessions: **238**
(unpriced/unknown-model assistant calls excluded from cost: 5)

**1. Per-session cost / tokens**

| | p50 | p90 | p99 | max |
|---|---|---|---|---|
| cost (USD) | 1.9993 | 8.5315 | 26.8807 | 64.4683 |
| tokens | 3,269,574 | 15,614,397 | 41,239,404 | 109,974,356 |

**2. Per-turn cost (USD)**

n=396  p50=1.3686  p90=5.6119  p99=10.7876  max=29.5731

**4. Model calls per turn (steps per turn)**

n=396  p50=25.0  p90=66.0  p99=130.3  max=178

### 3. Per calendar day total spend (UTC day, all sources combined + per source)

All sources combined: n_days=34  p50=73.5928  p90=191.4219  max=763.7183

pi: n_days=27  p50=8.2067  p90=18.9969  max=53.9354
Claude Code main: n_days=32  p50=68.4754  p90=154.5268  max=286.4218
Claude Code subagent: n_days=11  p50=32.1648  p90=117.2620  max=476.4468

Top 5 days by combined spend:

| day | total | pi | cc-main | cc-sub |
|---|---|---|---|---|
| 2026-09-23 | $763.7183 | $0.8497 | $286.4218 | $476.4468 |
| 2026-09-26 | $268.1547 | $0.0000 | $150.8927 | $117.2620 |
| 2026-09-24 | $223.3529 | $18.2795 | $97.7104 | $107.3630 |
| 2026-09-03 | $196.6876 | $8.3512 | $188.3364 | $0.0000 |
| 2026-09-28 | $179.1354 | $0.6027 | $94.6151 | $83.9176 |

### 5. Five most expensive sessions (across all sources)

| source | cost | duration | turns | model | notes |
|---|---|---|---|---|---|
| cc-main | $195.2129 | 14.2h | 167 | claude-opus-5-5 | max gap 3.5h |
| cc-main | $105.1238 | 18.2h | 46 | claude-opus-5 | max gap 8.9h |
| cc-main | $101.3921 | 7.1h | 41 | claude-sonnet-5 | no long gaps |
| cc-main | $100.4466 | 26.9h | 80 | claude-opus-5 | max gap 5.9h |
| cc-sub | $64.4683 | 1.5h | 3 | claude-opus-5-5 | max gap 1.0h |

### 6. Claude Code: subagent vs main spend

main=$2,545.9975  subagent=$924.2109  subagent share=26.6%

### pi: cost by provider (subscription vs API key)

| provider | payment | total cost |
|---|---|---|
| openai-codex | subscription | $247.2521 |
| opencode-go | API key | $39.2544 |
| anthropic | API key | $4.4177 |
| cursor | API key | $2.0929 |
| pi-claude-cli | subscription | $1.4831 |
| oc-sdk-go | API key | $1.3862 |
| opencode | API key | $0.0000 |

_Payment method is inferred from provider name only (no explicit auth-type field in the logs): `openai-codex` is the ChatGPT/Codex CLI subscription login, `pi-claude-cli` reuses the Claude Code CLI OAuth login; `anthropic`, `opencode-go`, `oc-sdk-go`, `opencode`, `cursor` are API-key-routed (OpenRouter-style / Cursor). Dollar figures for subscription providers are still the API-equivalent cost the harness logs, not actual out-of-pocket spend._

