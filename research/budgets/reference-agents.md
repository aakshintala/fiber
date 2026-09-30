# How pi, Codex and Claude Code show tokens/cost, and their spend limits

Method: read primary sources on this machine only (docs, `--help`, and
`strings` on the shipped binaries/bundles). No claim below is from memory.
Line numbers into `strings` dumps are from this session's temp files and
will not match a re-run; re-derive with the commands shown.

## pi (`/opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent`)

**1. Interactive UI / commands.** The footer shows "the current folder,
session, model, context usage, and accumulated usage and cost" (docs/usage.md
line 9, line 25). `/session` "shows its file, ID, message count, token usage,
and cost" (docs/sessions.md:16). No exact footer string format is in the
unminified docs; the minified TUI bundle (`dist/bundle/chunks/*.js`) was not
searched further for the literal render string — treat the footer's exact
formatting as "not found" beyond "shows usage and cost".

RPC command `get_session_stats` (docs/rpc-commands.md:520-559) returns:
```
tokens: {input, output, cacheRead, cacheWrite, total}
cost: 0.45                      // a single USD number
contextUsage: {tokens, contextWindow, percent}
```
"`tokens` and `cost` include assistant messages, usage reported by tools, and
compaction/branch-summary generation across the full session."
(rpc-commands.md:559)

**2. Cost computation.** From a static price table, not provider-reported
cost. `calculateCost(model, usage)` in
`node_modules/@earendil-works/pi-ai/dist/models.js:533-552` multiplies
provider-reported token counts by `model.cost.{input,output,cacheRead,
cacheWrite}` (USD per million tokens, doc says so at rpc-commands.md:832),
with a tiered-rate lookup (`model.cost.tiers`) and a 2x-input rule for
Anthropic 1h cache writes. JSON/RPC streaming events also carry a live
`usage.cost` object computed the same way (docs/json.md:98).

**3. Subscription / OAuth accounts.** pi supports OAuth login per-provider
(`/login`, docs/providers.md:5-18) and stores tokens in `auth.json`. Searched
`models.generated.js` and `dist/` for a subscription/zero-cost cost override
(`grep -i "oauth\|subscription"` against the cost tables) and found nothing
that zeroes or tags cost for OAuth/subscription auth — cost is computed the
same price-table way regardless of auth method. **Not found**: any "(sub)"
marker or hidden-cost behavior for OAuth logins.

**4. Subagents.** Searched all of `docs/` for "subagent" — **not found**.
pi's docs never use the term; it has no subagent/delegate concept to roll
usage into a parent total (its "extension" and "delegate" mentions are about
tool implementations, e.g. containerization.md:16, not spawned agents).

**5. Headless / JSON mode.** `pi --mode json` emits one JSONL event per
step (docs/json.md). Each `message_update` carries the cumulative
per-response `usage` (input/output/cacheRead/cacheWrite/totalTokens/cost)
(docs/json.md:95-98). There is no single closing "final totals" event in the
event list (`agent_end`/`agent_settled`, docs/json.md:44-56) — an RPC/JSON
client accumulates totals itself, or calls `get_session_stats` for the
authoritative session-wide number.

**6. Spend budget/limit.** Searched `docs/*.md` and `dist/*.js` for
`budget|spend|max.turns|max.cost|spendLimit|costLimit` (see commands run).
**Not found**: pi has no dollar spending cap, no `--max-turns`, no
`--max-budget-usd` equivalent. The only "budget" hits are unrelated
(compaction *token* budgets: docs/compaction.md, docs/settings.md:15
`thinkingBudgets`).

## Codex (`~/.codex/packages/standalone/current/bin/codex`, standalone build)

**1. Interactive UI.** Codex's TUI has a configurable status line
(`/status` config, strings dump around "Configure Status Line" /
"Select which items to display in the status line"). Its selectable fields
(one long strings run, `strings -n 8` on the binary) include, verbatim:
- "Percentage of context window remaining (omitted when unknown)"
- "Percentage of context window used (omitted when unknown)"
- "Remaining usage on the primary usage limit (omitted when unavailable)"
- "Remaining usage on the secondary usage limit (omitted when unavailable)"
- "Total tokens used in session (omitted when zero)"
- "Total input tokens used in session"
- "Total output tokens used in session"
- "Estimated current-thread credits (Enterprise workspaces only; omitted when unavailable)"
- "Estimated current-thread cost in USD (Enterprise workspaces only; omitted when unavailable)"

So: **USD cost is only ever shown for Enterprise workspaces, as an
estimate**; everyone else sees token counts and a "usage limit" percentage
(the ChatGPT plan's 5‑hour/weekly allowance), never a dollar figure. Other
literal strings found: `"Token usage: total=", " input=", " output="`
(a printed line, footer or `/status` summary) and `"% context left"`,
`"You have $ tokens left in this context window."` (a template with a
token-count placeholder, not a dollar amount — "$" here is the template's
own count-substitution marker in context, confirm before reusing).

**2. Cost computation.** Only the Enterprise "estimated cost in USD" string
exists; no price table or per-token USD-rate constants were found in the
binary strings (searched `usd|per_1m|price_per|cost_per_token|dollars`).
Everywhere else Codex tracks and shows only token counts and rate-limit
percentages — it does not compute a dollar cost for API-key/plan users.
Plan tiers found in the binary's enum strings: `free, go, plus, pro, lite,
promax, team, self_serve_business_prolite, self_serve_business_usage_based,
business, ent26, enterprise_cbp_automation, enterprise_cbp_usage_based,
enterprise, edu, edu_plus, edu_pro`.

**3. Subscription (ChatGPT login) accounts.** Cost is not shown at all for
non-Enterprise plans (see above) — not hidden-with-a-flag, simply absent
from the model; only token counts + rate-limit percentage are surfaced.
`RateLimitSnapshot` (struct fields: `limit_name, normal_model_slug, primary,
secondary, credits, individual_limits, spend_control_reached, plan_type,
rate_limit_reached_type`) is the structure behind `/status`'s "primary
usage limit" / "secondary usage limit" fields, and behind
`account/rateLimits/updated` app-server events.

**4. Subagents.** Codex has a `spawn_agent` tool ("This spawn_agent tool
provides you access to sub-agents that inherit your current model by
default…", found verbatim in the tool-description strings) plus
`assign_agent_task`, `wait_agent`, `close_agent` tool-call kinds, and a
`multi_agent` feature flag. Token usage per turn (`TurnCompletedEvent.usage`:
`input_tokens, cached_input_tokens, cache_write_input_tokens, output_tokens,
reasoning_output_tokens`) is the structure Codex reports over its app-server
protocol; it is emitted per turn, including sub-agent turns, over the same
`turn/completed` notification stream, but **not found**: any single field
that explicitly sums spawned-agent usage into a parent/session total (no
`aggregate_usage` or similar was found). Given cost isn't computed/shown
outside Enterprise anyway, this mostly matters for token counts, and no
roll-up field for those was found either — status-line "total tokens used in
session" is undocumented as to whether it includes sub-agent tokens.

**5. Headless / programmatic (`codex exec --json`).** `codex exec --json`
prints app-server-style JSONL events. Relevant ones seen in the protocol
method-name string table: `thread/tokenUsage/updated`, `turn/started`,
`turn/completed` (with the `usage` object above), `item/completed`. No
`total_cost_usd` or similar field exists in this event set — headless mode
reports token totals, not cost, for non-Enterprise use.

**6. Spend budget/limit.** Two distinct mechanisms found:
- **Token budget for the "goal" extension** (`ext/goal/src/tool.rs`,
  config `GoalsToml.max_goal_token_budget`, tool `update_goal`). Prompt
  templates embedded in the binary read: `"Budbudget:\n- Tokens used:
  {{ tokens_used }}\n- Token budget: {{ token_budget }}"` and the tool
  rejects non-positive values: `"goal budgets must be positive when
  provided"`. This is a per-goal (sub-task) token ceiling the model manages
  itself, not a CLI flag and not a dollar figure.
- **`spend_control_reached`** — a boolean on `RateLimitSnapshot`, and
  `SpendControlLimitSnapshot {used, remaining_percent}` — an account/workspace-level
  spend control (consumer/business plan usage-based billing guardrail),
  surfaced through `account/rateLimits/updated`; exact user-facing stop text
  was not found in the strings dump (only the field names were).
- `codex --help` / `codex exec --help`: **not found** — no `--max-turns`,
  `--max-budget`, or `--max-cost` flag exists on the CLI (checked full
  `--help` output for both `codex` and `codex exec`). `max_output_tokens` is
  a per-tool-call output-size cap (defaults to 10000 tokens, `exec` tool),
  not a session/turn budget.

## Claude Code (`~/.local/share/claude/versions/2.1.285`, plus `claude --help`)

**1. Interactive UI / commands.** `/cost` and `/stats` both now resolve to
`/usage` (verbatim: "Alternate names that resolve to this command (e.g.,
/cost and /stats both resolve to /usage)"). The session-exit summary block
(also what `/usage`/`/cost` show) is, verbatim, column-aligned:
```
Total cost:
Total duration (API):
Total duration (wall):
Total code changes:     <N> added, <N> removed
Usage:                  0 input, 0 output, 0 cache read, 0 cache write
Usage by model:
```
`/usage` also attributes usage to subagents/skills/plugins/MCP servers, e.g.
verbatim: `"% of your usage came from subagents under \""`, `"% of your usage
came from the MCP server \""`, `"Plugin skill-listing footprint"`, and offers
tips like `"If this runs frequently, consider configuring its subagents with
a cheaper model or tightening their prompts."` — this is a genuine usage
attribution breakdown, not just a total.

**2. Cost computation.** `--max-budget-usd` docs say "Maximum dollar amount
to spend on API calls" and the runtime checks the running total is numeric:
`"Session cost is not a number (a usage or pricing fault upstream); refusing
to continue under --max-budget-usd "` — implying cost is computed
client-side per turn (a pricing/price-table computation) rather than only
trusted from a provider-native field, since it can be found to be "not a
number" (a fault state) rather than simply absent.

**3. Subscription (Claude Pro/Max login).** Cost is not simply hidden or
"(sub)"-tagged; once a subscription's included quota is exhausted, Claude
Code switches to a separate **usage-credits** system with its own dollar
limits, verbatim strings:
- `"choose: continue on usage credits or switch models"`
- `"Now using usage credits"`, `"Turn on usage credits"`
- `"You can set a maximum amount you can spend on usage credits per month."`
- `"Set your monthly spend limit to"`, `"Monthly limit: "`, `"unlimited"`,
  `"This spend limit goes into effect immediately."`
- `"Automatically buy more usage credits when your balance is low."` /
  `"When usage credit balance falls below:"` (auto-recharge threshold)
- `/usage-credits` slash command to manage this ("Set up usage credits on
  claude.ai", "Manage usage credits on claude.ai", "Request usage credits
  from your admin")
So for subscription logins, Claude Code's own dollar-cost figure is not the
primary signal — usage credits (with their own monthly spend cap) are.

**4. Subagents.** Confirmed rolled up. The JSON result schema's internal doc
comment for `subagentStats` states verbatim: `"Subagents started through
the Agent tool in this session, as running totals ... Cumulative like
modelUsage: read the latest result rather than summing across results ...
as do its total_cost_usd, duration_api_ms and modelUsage"` — i.e. the
top-level `total_cost_usd` includes subagent (Task-tool) spend, and
`subagentStats` gives the per-category breakdown (spawned/completed/failed/
killed/by depth/by agent type). Explicitly excluded from this count:
"forked skills, workflows, teammates and other internal agents." A remote
(`isolation: remote`) subagent "counts as spawned but never reports an
outcome."

**5. Headless / programmatic mode.** The final JSON result
(`--output-format json`, or the last `stream-json` result message) carries,
verbatim field names found in the binary: `total_cost_usd (totalCostUsd)`,
`duration_api_ms (durationApiMs)`, `num_turns`, `usage`, `modelUsage`,
`subagentStats`, `is_error`, `result subtype=success`. On a budget/turn stop
it instead carries `error_max_budget_usd` / `error_max_turns` as the result
subtype/error code.

**6. Spend budget / turn limit.** `claude --help` (v2.1.285) currently shows
only:
```
--max-budget-usd <amount>   Maximum dollar amount to spend on API calls (only works with --print)
```
Validation/behavior strings found: `"--max-budget-usd must be a positive
number greater than 0"`; on hit: `"print budget halt: this run's spend "
+ "<$X>" + " reached --max-budget-usd " + "<$Y>" + "; stopping background
agents"`, and `"Reached maximum budget ($<amount>)"`. It also stops any
running Task-tool subagents (documented on the `subagentStats` "stopped
before finishing" reasons: `"the --max-budget-usd halt, the sweep of
background subagents when an SDK or IDE client interrupts, or -p giving up
on a background subagent still running at its wait ceiling"`).

Two flags exist as embedded help text in the binary but are **not** printed
by `claude --help` in this installed version (2.1.285) — worth flagging as
either feature-gated or newly added and not yet surfaced:
```
--max-turns <turns>     Maximum number of agentic turns in non-interactive mode.
                        This will early exit the conversation after the specified
                        number of turns. (only works with --print)
--task-budget <tokens>  API-side task budget in tokens (output_config.task_budget)
```
Corresponding runtime strings confirm `--max-turns` is real and wired up
even though hidden from `--help`: `"error_max_turns"`, `"Reached maximum
number of turns ("`, `"max_turns_reached"`, `"hit_max_turns"`.

There is also a distinct, smaller-scope **turn token budget** (not a dollar
cap): `"Output tokens spent this turn toward the token budget."` / `"Turn
token-budget ceiling."` / `"Budget-nudge count this turn."` — an output-size
governor per turn, unrelated to `--max-budget-usd`.

`CLAUDE_CODE_MAX_CONCURRENT_SUBAGENTS` env var caps subagent concurrency
(not spend).

## Cross-agent summary

| | pi | Codex | Claude Code |
|---|---|---|---|
| Shows $ cost in UI | Yes (footer, `/session`, RPC `get_session_stats.cost`) | Only "Enterprise workspaces" estimate; else tokens + rate-limit % only | Yes (`/usage` aka `/cost`/`/stats`, exit summary) |
| Cost source | Price table × provider token usage | No price table found; not computed for non-Enterprise | Client-side computed (price table implied; validated as "a number") |
| Subscription/OAuth cost display | Same price-table cost, no override found | No $ cost shown at all (ChatGPT plans use rate-limit %/credits, not $) | Falls back to "usage credits" system with its own monthly $ cap once included quota is used |
| Subagents roll into total | No subagent concept in pi | Turn-level usage per agent; no explicit "rolled into parent total" field found | Yes, explicitly documented (`subagentStats` + `total_cost_usd`) |
| Headless final totals | No single final-totals event; poll `get_session_stats` | `turn/completed.usage` (tokens only, no cost) | `total_cost_usd`, `duration_api_ms`, `num_turns`, `usage`, `modelUsage`, `subagentStats` |
| Spend/turn limit flag | Not found | Not found on CLI; account-level `spend_control_reached` / goal-level `max_goal_token_budget` (token, not $) exist | `--max-budget-usd` (documented + enforced); `--max-turns` and `--task-budget` exist in binary but hidden from `--help` in 2.1.285 |
