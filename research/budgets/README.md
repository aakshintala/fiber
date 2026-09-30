# Budgets: spend, and what the reference agents do

Evidence for
[Token and cost display, and spending budgets](https://github.com/aakshintala/fiber/issues/192).

- [spend.md](spend.md): the owner's spend from pi and Claude Code logs,
  2026-07-25 to 2026-09-29, made by [spend.py](spend.py) (`--selftest` checks
  it). Claude Code has no cost field, so its figures are tokens at API prices.
- [reference-agents.md](reference-agents.md): how pi, codex and Claude Code
  show usage and cost, and which have a spending limit.

## Findings

- Nearly all the owner's spend runs on subscriptions. pi logged $296, $247 of
  it through the ChatGPT/codex login. Claude Code comes to $3,470 at API
  prices over the same 34 days, on a $100 Max plan.
- Per session at API prices (p50, p90, max): Claude Code main $3.88, $21.67,
  $195.21; Claude Code subagents $2.00, $8.53, $64.47; pi $0.05, $0.93,
  $16.73. Subagents are 26.6% of Claude Code spend.
- Two of the five most expensive sessions had gaps of 5.9 and 8.9 hours with
  no prompt, which is spend while nobody watched.
- Claude Code has `--max-budget-usd`, for `--print` only. It ends the run with
  "print budget halt: this run's spend $X reached --max-budget-usd $Y;
  stopping background agents", and its `total_cost_usd` includes subagents.
  Codex shows no dollars outside Enterprise and has no spending flag. pi
  prices every call from a static table, subscriptions included, and has no
  limit.
- Claude Code 2.1.285 on a Max login: the `system` `init` line has
  `apiKeySource` `none`, and the `result` line still reports
  `total_cost_usd` (0.0228 for one Haiku reply), an API-price estimate.
